//! Channel targeting + routing primitives for N-channel processing.
//!
//! [`ChannelMask`] selects which channels a single EQ band applies to. This is
//! how per-channel EQ is expressed without a separate filter bank per channel:
//! the chain keeps one flat band list and skips channels a band's mask excludes.
//! Each band already owns per-channel biquad state, so a band masked to one
//! channel only advances that channel's state.
//!
//! [`ChannelMatrix`] is an `out_ch × in_ch` mixing matrix applied as the final
//! chain stage. It covers identity passthrough, L/R (or any pair) swap, arbitrary
//! permutation, channel duplication / drop, and up/downmix — i.e. routing any
//! input channel to any output channel. `apply` is allocation-free.

/// Bitset over channel indices. Supports up to 64 channels, far beyond any real
/// device. [`ChannelMask::ALL`] matches every channel regardless of the live
/// channel count, so a band defined before the device layout is known still
/// applies everywhere (the back-compatible default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMask(u64);

/// Channels beyond this index can't be represented in the [`ChannelMask`] bitset.
pub const MAX_CHANNELS: usize = 64;

impl ChannelMask {
    /// Matches every channel (default for a band with no explicit target).
    pub const ALL: ChannelMask = ChannelMask(u64::MAX);
    /// Matches no channel (an effectively muted band).
    pub const NONE: ChannelMask = ChannelMask(0);

    /// Mask with a single channel index set. Indices ≥ [`MAX_CHANNELS`] yield an
    /// empty mask rather than wrapping the shift.
    pub fn single(ch: usize) -> Self {
        if ch >= MAX_CHANNELS {
            ChannelMask(0)
        } else {
            ChannelMask(1u64 << ch)
        }
    }

    /// Build from an iterator of channel indices (indices ≥ [`MAX_CHANNELS`] are
    /// ignored).
    pub fn from_indices<I: IntoIterator<Item = usize>>(it: I) -> Self {
        let mut bits = 0u64;
        for ch in it {
            if ch < MAX_CHANNELS {
                bits |= 1u64 << ch;
            }
        }
        ChannelMask(bits)
    }

    /// Raw bitset — for serialization at the IPC boundary.
    pub fn bits(self) -> u64 {
        self.0
    }

    /// Reconstruct from a raw bitset.
    pub fn from_bits(bits: u64) -> Self {
        ChannelMask(bits)
    }

    /// Does this mask include channel `ch`?
    #[inline]
    pub fn contains(self, ch: usize) -> bool {
        ch < MAX_CHANNELS && (self.0 & (1u64 << ch)) != 0
    }

    /// Copy with channel `ch` added.
    pub fn with(self, ch: usize) -> Self {
        ChannelMask(self.0 | ChannelMask::single(ch).0)
    }

    /// Copy with channel `ch` removed.
    pub fn without(self, ch: usize) -> Self {
        ChannelMask(self.0 & !ChannelMask::single(ch).0)
    }

    /// True when no channel is selected.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every channel in `0..channels` is selected (the band is global
    /// for the current layout). `ALL` is always global.
    pub fn is_global(self, channels: usize) -> bool {
        if channels == 0 || channels > MAX_CHANNELS {
            return self.0 == u64::MAX;
        }
        let full = if channels == MAX_CHANNELS {
            u64::MAX
        } else {
            (1u64 << channels) - 1
        };
        (self.0 & full) == full
    }
}

impl Default for ChannelMask {
    /// A band with no explicit target applies to every channel.
    fn default() -> Self {
        ChannelMask::ALL
    }
}

/// An `out_ch × in_ch` channel mixing matrix applied as the final chain stage.
///
/// `out[o] = Σ_i gain[o][i] · in[i]`, stored row-major: `gains[o * in_ch + i]`.
/// Covers identity, swap, permutation, duplication, drop and up/downmix.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMatrix {
    in_ch: usize,
    out_ch: usize,
    /// Row-major, length `out_ch * in_ch`. Row `o` mixes all inputs into output `o`.
    gains: Vec<f64>,
}

impl ChannelMatrix {
    /// Build from explicit row-major gains. Returns `None` unless
    /// `gains.len() == out_ch * in_ch` (and neither dimension is zero).
    pub fn new(in_ch: usize, out_ch: usize, gains: Vec<f64>) -> Option<Self> {
        if in_ch == 0 || out_ch == 0 || gains.len() != in_ch.saturating_mul(out_ch) {
            return None;
        }
        Some(Self {
            in_ch,
            out_ch,
            gains,
        })
    }

    /// Square identity (`out == in`, unity diagonal).
    pub fn identity(channels: usize) -> Self {
        let mut gains = vec![0.0; channels * channels];
        for c in 0..channels {
            gains[c * channels + c] = 1.0;
        }
        Self {
            in_ch: channels,
            out_ch: channels,
            gains,
        }
    }

    /// Identity with channels `a` and `b` swapped — e.g. L/R swap is
    /// `swap(2, 0, 1)`. Out-of-range or equal indices leave identity unchanged.
    pub fn swap(channels: usize, a: usize, b: usize) -> Self {
        let mut m = Self::identity(channels);
        if a < channels && b < channels && a != b {
            m.gains[a * channels + a] = 0.0;
            m.gains[b * channels + b] = 0.0;
            m.gains[a * channels + b] = 1.0;
            m.gains[b * channels + a] = 1.0;
        }
        m
    }

    pub fn in_ch(&self) -> usize {
        self.in_ch
    }

    pub fn out_ch(&self) -> usize {
        self.out_ch
    }

    pub fn gains(&self) -> &[f64] {
        &self.gains
    }

    /// True when this is the square identity — the chain can then skip routing
    /// entirely and process in place (the zero-cost common path).
    pub fn is_identity(&self) -> bool {
        if self.in_ch != self.out_ch {
            return false;
        }
        self.gains.iter().enumerate().all(|(k, &g)| {
            let (o, i) = (k / self.in_ch, k % self.in_ch);
            g == if o == i { 1.0 } else { 0.0 }
        })
    }

    /// Apply the matrix. `src` is `frames * in_ch` interleaved; `dst` must hold at
    /// least `frames * out_ch` samples (extra trailing capacity is left
    /// untouched). Allocation-free; both buffers are owned by the caller.
    pub fn apply(&self, src: &[f64], dst: &mut [f64]) {
        if self.in_ch == 0 || self.out_ch == 0 {
            return;
        }
        // Clamp the frame count to BOTH buffers' capacities. The matrix may be
        // mismatched against the live buffers (e.g. an in_ch that doesn't equal
        // the stream's channel count); deriving frames from `src` alone could
        // then overrun `dst`. Bounding to the min makes `apply` panic-free for
        // any matrix/buffer pair — defense in depth behind the install-time
        // validation in the daemon.
        let frames = (src.len() / self.in_ch).min(dst.len() / self.out_ch);
        for frame in 0..frames {
            let s = &src[frame * self.in_ch..frame * self.in_ch + self.in_ch];
            let d = &mut dst[frame * self.out_ch..frame * self.out_ch + self.out_ch];
            for (o, d_o) in d.iter_mut().enumerate() {
                let row = &self.gains[o * self.in_ch..o * self.in_ch + self.in_ch];
                *d_o = row.iter().zip(s).map(|(g, x)| g * x).sum();
            }
        }
    }
}
