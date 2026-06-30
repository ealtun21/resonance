// SPDX-License-Identifier: LGPL-3.0-or-later
//
// Rust port of peqdb/autoeq-c (https://github.com/peqdb/autoeq-c),
// Copyright (C) 2026 PEQdB Inc., LGPL-3.0-or-later. The initialization and
// adaptive-smoothing strategies were adapted there from jaakkopasanen/AutoEq
// (MIT). This is a faithful translation kept in its own crate so the copyleft
// licence stays isolated from the rest of Resonance.
//
//! `AutoEQ`: fit a stack of biquad filters (peaking + low/high shelf) so a
//! measured headphone response, once EQ'd, matches a target. Operates on a
//! fixed 384-point log-frequency grid (20 Hz–20 kHz) in dB; an `AdaBelief`
//! optimizer with analytic biquad gradients minimises mean-squared error, with
//! adaptive treble smoothing so resonance peaks aren't flattened.

use std::f32::consts::{FRAC_1_SQRT_2, LN_2, LN_10, PI};

/// Number of log-spaced grid points the fitter works on.
pub const K: usize = 384;
const MAX_N: usize = 32;
const FS: f32 = 48_000.0;
const F0: f32 = 20.0;
const F1: f32 = 20_000.0;

#[inline]
fn exp10(x: f32) -> f32 {
    (LN_10 * x).exp()
}
#[inline]
fn sq(x: f32) -> f32 {
    x * x
}
#[inline]
fn clip(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}
/// Clamp in place; report whether it changed (to zero optimizer momentum).
#[inline]
// float_cmp: intentional — compare clamped value to stored original to detect a change.
#[allow(clippy::float_cmp)]
fn limit(x: &mut f32, lo: f32, hi: f32) -> bool {
    let orig = *x;
    *x = clip(*x, lo, hi);
    *x != orig
}

/// The 384 log-spaced grid frequencies (Hz) the fitter expects its inputs on.
#[must_use]
pub fn log_freqs() -> Vec<f32> {
    let (l0, l1) = (F0.ln(), F1.ln());
    let lr = l1 - l0;
    (0..K)
        .map(|k| (l0 + lr / (K as f32 - 1.0) * k as f32).exp())
        .collect()
}

// ── Public result types ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BandKind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, Debug)]
pub struct EqFilter {
    pub kind: BandKind,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Clone, Debug)]
pub struct AutoEqResult {
    /// Headroom preamp (≤ 0 dB) so the boosted result doesn't clip.
    pub preamp_db: f64,
    pub filters: Vec<EqFilter>,
}

/// Coupler/rig smoothing profile — picks how aggressively the treble is smoothed
/// before fitting (in-ear preserves the ~8 kHz coupler peak).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Smoothing {
    InEar,
    OverEar,
    None,
}

// ── Filter type + biquad coefficients with analytic derivatives ─────────────

#[derive(Clone, Copy, PartialEq)]
enum Type {
    Pk,
    Lsc,
    Hsc,
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a0: f32,
    a1: f32,
    a2: f32,
    db0_da: f32,
    db0_dalpha: f32,
    db0_dcos: f32,
    db1_da: f32,
    db1_dcos: f32,
    db2_da: f32,
    db2_dalpha: f32,
    db2_dcos: f32,
    da0_da: f32,
    da0_dalpha: f32,
    da0_dcos: f32,
    da1_da: f32,
    da1_dcos: f32,
    da2_da: f32,
    da2_dalpha: f32,
    da2_dcos: f32,
}

fn pk(a: f32, cos_w: f32, alpha: f32) -> Biquad {
    let r_a = 1.0 / a;
    Biquad {
        b0: a * alpha + 1.0,
        db0_da: alpha,
        db0_dalpha: a,
        db0_dcos: 0.0,
        b1: -2.0 * cos_w,
        db1_da: 0.0,
        db1_dcos: -2.0,
        b2: -a * alpha + 1.0,
        db2_da: -alpha,
        db2_dalpha: -a,
        db2_dcos: 0.0,
        a0: (a + alpha) * r_a,
        da0_da: -alpha * sq(r_a),
        da0_dalpha: r_a,
        da0_dcos: 0.0,
        a1: -2.0 * cos_w,
        da1_da: 0.0,
        da1_dcos: -2.0,
        a2: (a - alpha) * r_a,
        da2_da: alpha * sq(r_a),
        da2_dalpha: -r_a,
        da2_dcos: 0.0,
    }
}

fn lsc(a: f32, cos_w: f32, alpha: f32) -> Biquad {
    let p1 = a + 1.0;
    let m1 = a - 1.0;
    let sqrt_a = a.sqrt();
    let k = 2.0 * sqrt_a * alpha;
    let dk_da = alpha / sqrt_a;
    let dk_dalpha = 2.0 * sqrt_a;
    Biquad {
        b0: a * (-cos_w * m1 + k + p1),
        db0_da: a * dk_da - a * cos_w + a - cos_w * m1 + k + p1,
        db0_dalpha: a * dk_dalpha,
        db0_dcos: -a * m1,
        b1: 2.0 * a * (-cos_w * p1 + m1),
        db1_da: -2.0 * a * cos_w + 2.0 * a - 2.0 * cos_w * p1 + 2.0 * m1,
        db1_dcos: -2.0 * a * p1,
        b2: a * (-cos_w * m1 - k + p1),
        db2_da: -a * dk_da - a * cos_w + a - cos_w * m1 - k + p1,
        db2_dalpha: -a * dk_dalpha,
        db2_dcos: -a * m1,
        a0: cos_w * m1 + k + p1,
        da0_da: dk_da + cos_w + 1.0,
        da0_dalpha: dk_dalpha,
        da0_dcos: m1,
        a1: -2.0 * cos_w * p1 - 2.0 * m1,
        da1_da: -2.0 * cos_w - 2.0,
        da1_dcos: -2.0 * p1,
        a2: cos_w * m1 - k + p1,
        da2_da: -dk_da + cos_w + 1.0,
        da2_dalpha: -dk_dalpha,
        da2_dcos: m1,
    }
}

fn hsc(a: f32, cos_w: f32, alpha: f32) -> Biquad {
    let p1 = a + 1.0;
    let m1 = a - 1.0;
    let sqrt_a = a.sqrt();
    let k = 2.0 * sqrt_a * alpha;
    let dk_da = alpha / sqrt_a;
    let dk_dalpha = 2.0 * sqrt_a;
    Biquad {
        b0: a * (cos_w * m1 + k + p1),
        db0_da: a * dk_da + a * cos_w + a + cos_w * m1 + k + p1,
        db0_dalpha: a * dk_dalpha,
        db0_dcos: a * m1,
        b1: -2.0 * a * (cos_w * p1 + m1),
        db1_da: -2.0 * a * cos_w - 2.0 * a - 2.0 * cos_w * p1 - 2.0 * m1,
        db1_dcos: -2.0 * a * p1,
        b2: a * (cos_w * m1 - k + p1),
        db2_da: -a * dk_da + a * cos_w + a + cos_w * m1 - k + p1,
        db2_dalpha: -a * dk_dalpha,
        db2_dcos: a * m1,
        a0: -cos_w * m1 + k + p1,
        da0_da: dk_da - cos_w + 1.0,
        da0_dalpha: dk_dalpha,
        da0_dcos: -m1,
        a1: -2.0 * cos_w * p1 + 2.0 * m1,
        da1_da: 2.0 - 2.0 * cos_w,
        da1_dcos: -2.0 * p1,
        a2: -cos_w * m1 - k + p1,
        da2_da: -dk_da - cos_w + 1.0,
        da2_dalpha: -dk_dalpha,
        da2_dcos: -m1,
    }
}

fn biquad(t: Type, a: f32, cos_w: f32, alpha: f32) -> Biquad {
    match t {
        Type::Pk => pk(a, cos_w, alpha),
        Type::Lsc => lsc(a, cos_w, alpha),
        Type::Hsc => hsc(a, cos_w, alpha),
    }
}

/// Add one filter's magnitude response (dB) at every grid frequency to `y`.
fn spectrum(t: Type, f0: f32, gain: f32, q: f32, f: &[f32], y: &mut [f32]) {
    let a = exp10(gain / 40.0);
    let w0 = 2.0 * PI / FS * f0;
    let cos_w = w0.cos();
    let sin_w = w0.sin();
    let alpha = sin_w * 0.5 / q;
    let s = biquad(t, a, cos_w, alpha);

    let b_x0 = sq(s.b0 + s.b1 + s.b2);
    let b_x1 = -4.0 * (s.b0 * s.b1 + 4.0 * s.b0 * s.b2 + s.b1 * s.b2);
    let b_x2 = 16.0 * s.b0 * s.b2;
    let a_x0 = sq(s.a0 + s.a1 + s.a2);
    let a_x1 = -4.0 * (s.a0 * s.a1 + 4.0 * s.a0 * s.a2 + s.a1 * s.a2);
    let a_x2 = 16.0 * s.a0 * s.a2;

    for k in 0..K {
        let phi = sq((PI / FS * f[k]).sin());
        let b_poly = b_x0 + phi * (b_x1 + phi * b_x2);
        let a_poly = a_x0 + phi * (a_x1 + phi * a_x2);
        y[k] += 10.0 * (b_poly / a_poly).log10();
    }
}

// ── Initialization (adapted from jaakkopasanen/AutoEq) ──────────────────────

#[derive(Clone, Copy)]
struct Lim {
    lo: f32,
    hi: f32,
}

#[derive(Clone, Copy)]
struct Filter {
    f0: f32,
    gain: f32,
    q: f32,
}

#[derive(Clone, Copy)]
struct Peak {
    width: f32,
    height: f32,
    idx: i32,
}

/// scipy-style largest-peak detector (`find_peaks` → prominences → widths).
// float_cmp: intentional — exact equality walks flat plateaus (scipy find_peaks port).
#[allow(clippy::float_cmp)]
fn largest_peak(x: &[f32], f: &[f32], lim: Lim) -> Peak {
    let i_max = K - 1;
    let mut peaks: Vec<usize> = Vec::new();

    let mut i = 1;
    while i < i_max {
        if f[i] < lim.lo || f[i] > lim.hi || x[i - 1] >= x[i] {
            i += 1;
            continue;
        }
        let mut i_ahead = i + 1;
        while i_ahead < i_max && x[i_ahead] == x[i] {
            i_ahead += 1;
        }
        if x[i_ahead] < x[i] {
            peaks.push((i + i_ahead - 1) / 2);
            i = i_ahead;
        } else {
            i += 1;
        }
    }

    let n = peaks.len();
    let mut prominences = vec![0.0f32; n];
    let mut left_bases = vec![0usize; n];
    let mut right_bases = vec![0usize; n];

    for p in 0..n {
        let peak = peaks[p];
        let x_peak = x[peak];

        left_bases[p] = peak;
        let mut left_min = x_peak;
        let mut i = peak as i32;
        while i >= 0 && x[i as usize] <= x_peak {
            if x[i as usize] < left_min {
                left_min = x[i as usize];
                left_bases[p] = i as usize;
            }
            i -= 1;
        }

        right_bases[p] = peak;
        let mut right_min = x_peak;
        let mut i = peak;
        while i <= i_max && x[i] <= x_peak {
            if x[i] < right_min {
                right_min = x[i];
                right_bases[p] = i;
            }
            i += 1;
        }

        prominences[p] = x_peak - left_min.max(right_min);
    }

    let mut largest = Peak {
        width: 0.0,
        height: 0.0,
        idx: -1,
    };
    let mut largest_size = 0.0f32;

    for p in 0..n {
        let i_min = left_bases[p];
        let i_max_p = right_bases[p];
        let peak = peaks[p];
        let x_peak = x[peak];
        let height = x_peak - 0.5 * prominences[p];

        let mut i = peak;
        while i_min < i && height < x[i] {
            i -= 1;
        }
        let mut left_ip = i as f32;
        if x[i] < height {
            left_ip += (height - x[i]) / (x[i + 1] - x[i]);
        }

        let mut i = peak;
        while i < i_max_p && height < x[i] {
            i += 1;
        }
        let mut right_ip = i as f32;
        if x[i] < height {
            right_ip -= (height - x[i]) / (x[i - 1] - x[i]);
        }

        let width = right_ip - left_ip;
        let size = width * x_peak;
        if size > largest_size {
            largest = Peak {
                idx: peak as i32,
                width,
                height: x_peak,
            };
            largest_size = size;
        }
    }

    largest
}

fn init_pk(y: &[f32], f: &[f32], lim_f0: Lim, lim_gain: Lim, lim_q: Lim) -> Filter {
    let mut rect = [0.0f32; K];
    for k in 0..K {
        rect[k] = y[k].max(0.0);
    }
    let peak = largest_peak(&rect, f, lim_f0);
    for k in 0..K {
        rect[k] = (-y[k]).max(0.0);
    }
    let dip = largest_peak(&rect, f, lim_f0);

    let p = if peak.width * peak.height > dip.width * dip.height {
        peak
    } else {
        dip
    };
    if p.idx < 0 {
        // Residual is flat in this band — contribute nothing.
        return Filter {
            f0: (lim_f0.lo * lim_f0.hi).sqrt(),
            gain: 0.0,
            q: 1.0,
        };
    }

    let f0 = f[p.idx as usize];
    let gain = if p.idx == peak.idx {
        peak.height
    } else {
        -dip.height
    };
    let bw = p.width * (f[1] / f[0]).log2();
    let bw_exp2 = bw.exp2();
    let q = bw_exp2.sqrt() / (bw_exp2 - 1.0);

    Filter {
        f0,
        gain: clip(gain, lim_gain.lo, lim_gain.hi),
        q: clip(q, lim_q.lo, lim_q.hi),
    }
}

fn init_shelf(t: Type, y: &[f32], f: &[f32], mut lim_f0: Lim, lim_gain: Lim) -> Filter {
    lim_f0.lo = lim_f0.lo.max(40.0);
    lim_f0.hi = lim_f0.hi.min(10_000.0);

    let high = t == Type::Hsc;
    let mut best = 0.0f32;
    let mut best_idx = 0i32;
    let mut acc = 0.0f32;
    for k in 0..K {
        let idx = if high { K - 1 - k } else { k };
        acc += y[idx];
        let avg = (acc / (k as f32 + 1.0)).abs();
        if avg > best {
            best = avg;
            best_idx = idx as i32;
        }
    }

    let mut f0 = f[best_idx as usize];
    let q = FRAC_1_SQRT_2;
    let _ = limit(&mut f0, lim_f0.lo, lim_f0.hi);

    let mut w = [0.0f32; K];
    spectrum(t, f0, 1.0, q, f, &mut w);
    let (mut p, mut c) = (0.0f32, 0.0f32);
    for k in 0..K {
        p += w[k] * y[k];
        c += w[k];
    }
    let gain = clip(p / c, lim_gain.lo, lim_gain.hi);

    Filter { f0, gain, q }
}

fn init(t: Type, y: &[f32], f: &[f32], lim_f0: Lim, lim_gain: Lim, lim_q: Lim) -> Filter {
    match t {
        Type::Pk => init_pk(y, f, lim_f0, lim_gain, lim_q),
        Type::Lsc | Type::Hsc => init_shelf(t, y, f, lim_f0, lim_gain),
    }
}

// ── Optimizer (AdaBelief) over a (log f, gain, bw) reparameterization ───────

fn q_to_bw(q: f32) -> f32 {
    2.0 / LN_2 * (0.5 / q).asinh()
}
fn bw_to_q(bw: f32) -> f32 {
    0.5 / (0.5 * LN_2 * bw).sinh()
}

/// Free parameters: `[lf_0..lf_{N-1}, gain.., bw.., amp]`.
struct Wrt {
    v: Vec<f32>,
}
impl Wrt {
    fn zeros(n: usize) -> Self {
        Wrt {
            v: vec![0.0; 3 * n + 1],
        }
    }
}

struct Scratch {
    pred: [f32; K],
    dl_dy: [f32; K],
    w0_v: Vec<f32>,
    dy_dw0: Vec<f32>,
    dy_dgain: Vec<f32>,
    dy_dbw: Vec<f32>,
}
impl Scratch {
    fn new(n: usize) -> Self {
        Scratch {
            pred: [0.0; K],
            dl_dy: [0.0; K],
            w0_v: vec![0.0; n],
            dy_dw0: vec![0.0; n * K],
            dy_dgain: vec![0.0; n * K],
            dy_dbw: vec![0.0; n * K],
        }
    }
}

struct Consts<'a> {
    types: &'a [Type],
    phi: &'a [f32],
    r: &'a [f32],
    n: usize,
}

/// Loss (MSE in dB) + its gradient into `g`. Faithful to autoeq-c `grad`.
// similar_names: dy_db0..dy_da2 mirror the b0..b2/a0..a2 coefficient partial-derivative math.
#[allow(clippy::similar_names)]
fn grad(c: &Consts, x: &Wrt, g: &mut Wrt, sc: &mut Scratch) -> f32 {
    let n = c.n;
    let r_k = 1.0 / K as f32;

    let pred_init = exp10(x.v[3 * n] / 10.0);
    for k in 0..K {
        sc.pred[k] = pred_init;
    }

    for nn in 0..n {
        let f0 = x.v[nn].exp();
        let gain = x.v[n + nn];
        let bw = x.v[2 * n + nn];

        let a = exp10(gain / 40.0);
        let w0 = 2.0 * PI / FS * f0;
        let cos_w = w0.cos();
        let sin_w = w0.sin();
        let k_q = (0.5 * LN_2 * bw).sinh();
        let alpha = sin_w * k_q;
        sc.w0_v[nn] = w0;

        let s = biquad(c.types[nn], a, cos_w, alpha);

        let da_dgain = a * LN_10 / 40.0;
        let dalpha_dw0 = cos_w * k_q;
        let dalpha_dbw = sin_w * (0.5 * LN_2 * bw).cosh() * 0.5 * LN_2;
        let dcos_dw0 = -sin_w;

        let b_x0 = sq(s.b0 + s.b1 + s.b2);
        let b_x1 = -4.0 * (s.b0 * s.b1 + 4.0 * s.b0 * s.b2 + s.b1 * s.b2);
        let b_x2 = 16.0 * s.b0 * s.b2;
        let a_x0 = sq(s.a0 + s.a1 + s.a2);
        let a_x1 = -4.0 * (s.a0 * s.a1 + 4.0 * s.a0 * s.a2 + s.a1 * s.a2);
        let a_x2 = 16.0 * s.a0 * s.a2;
        let ba = s.b0 + s.b1 + s.b2;
        let aa = s.a0 + s.a1 + s.a2;

        for k in 0..K {
            let phi_k = c.phi[k];
            let b_poly = b_x0 + phi_k * (b_x1 + phi_k * b_x2);
            let a_poly = a_x0 + phi_k * (a_x1 + phi_k * a_x2);
            sc.pred[k] *= b_poly / a_poly;

            let eight_phi2 = 8.0 * sq(phi_k);
            let two_phi = 2.0 * phi_k;
            let bm = 20.0 / LN_10 / b_poly;
            let am = -20.0 / LN_10 / a_poly;

            let dy_db0 = bm * (ba - two_phi * (s.b1 + 4.0 * s.b2) + eight_phi2 * s.b2);
            let dy_db1 = bm * (ba - two_phi * (s.b0 + s.b2));
            let dy_db2 = bm * (ba - two_phi * (4.0 * s.b0 + s.b1) + eight_phi2 * s.b0);
            let dy_da0 = am * (aa - two_phi * (s.a1 + 4.0 * s.a2) + eight_phi2 * s.a2);
            let dy_da1 = am * (aa - two_phi * (s.a0 + s.a2));
            let dy_da2 = am * (aa - two_phi * (4.0 * s.a0 + s.a1) + eight_phi2 * s.a0);

            let dy_da = dy_db0 * s.db0_da
                + dy_db1 * s.db1_da
                + dy_db2 * s.db2_da
                + dy_da0 * s.da0_da
                + dy_da1 * s.da1_da
                + dy_da2 * s.da2_da;
            let dy_dalpha = dy_db0 * s.db0_dalpha
                + dy_db2 * s.db2_dalpha
                + dy_da0 * s.da0_dalpha
                + dy_da2 * s.da2_dalpha;
            let dy_dcos = dy_db0 * s.db0_dcos
                + dy_db1 * s.db1_dcos
                + dy_db2 * s.db2_dcos
                + dy_da0 * s.da0_dcos
                + dy_da1 * s.da1_dcos
                + dy_da2 * s.da2_dcos;

            sc.dy_dw0[nn * K + k] = dy_dalpha * dalpha_dw0 + dy_dcos * dcos_dw0;
            sc.dy_dgain[nn * K + k] = dy_da * da_dgain;
            sc.dy_dbw[nn * K + k] = dy_dalpha * dalpha_dbw;
        }
    }

    let mut l = 0.0f32;
    let mut dl_dy_sum = 0.0f32;
    for k in 0..K {
        let d = 10.0 * sc.pred[k].log10() - c.r[k];
        l += sq(d);
        sc.dl_dy[k] = 2.0 * d;
        dl_dy_sum += sc.dl_dy[k];
    }
    l *= r_k;
    g.v[3 * n] = dl_dy_sum * r_k;

    for nn in 0..n {
        let (mut glf, mut ggain, mut gbw) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..K {
            glf += sc.dl_dy[k] * sc.dy_dw0[nn * K + k];
            ggain += sc.dl_dy[k] * sc.dy_dgain[nn * K + k];
            gbw += sc.dl_dy[k] * sc.dy_dbw[nn * K + k];
        }
        g.v[nn] = glf * r_k * sc.w0_v[nn];
        g.v[n + nn] = ggain * r_k;
        g.v[2 * n + nn] = gbw * r_k;
    }

    l
}

struct AdaBelief {
    m: Vec<f32>,
    s: Vec<f32>,
    b1: f32,
    b2: f32,
    b1t: f32,
    b2t: f32,
    eps: f32,
    eps_root: f32,
    lr: f32,
}
impl AdaBelief {
    fn new(w: usize) -> Self {
        AdaBelief {
            m: vec![0.0; w],
            s: vec![0.0; w],
            b1: 0.9,
            b2: 0.99,
            b1t: 0.9,
            b2t: 0.99,
            eps: 1e-12,
            eps_root: 1e-12,
            lr: 3e-2,
        }
    }
    fn step(&mut self, x: &mut Wrt, g: &Wrt) {
        for w in 0..x.v.len() {
            self.m[w] = self.b1 * self.m[w] + (1.0 - self.b1) * g.v[w];
            self.s[w] = self.b2 * self.s[w] + (1.0 - self.b2) * sq(g.v[w] - self.m[w]);
            let m_hat = self.m[w] / (1.0 - self.b1t);
            let s_hat = self.s[w] / (1.0 - self.b2t);
            let den = (s_hat + self.eps_root).sqrt() + self.eps;
            x.v[w] -= self.lr * m_hat / den;
        }
        self.b1t *= self.b1;
        self.b2t *= self.b2;
    }
}

#[allow(clippy::too_many_arguments)]
fn fit(
    steps: usize,
    types: &[Type],
    f0: &mut [f32],
    gain: &mut [f32],
    q: &mut [f32],
    amp: &mut f32,
    f0_lim: &[Lim],
    gain_lim: &[Lim],
    q_lim: &[Lim],
    n: usize,
    f: &[f32],
    r: &[f32],
) -> f32 {
    let lf_lim: Vec<Lim> = (0..n)
        .map(|i| Lim {
            lo: f0_lim[i].lo.ln(),
            hi: f0_lim[i].hi.ln(),
        })
        .collect();
    let bw_lim: Vec<Lim> = (0..n)
        .map(|i| Lim {
            lo: q_to_bw(q_lim[i].hi),
            hi: q_to_bw(q_lim[i].lo),
        })
        .collect();

    let phi: Vec<f32> = (0..K).map(|k| sq((PI / FS * f[k]).sin())).collect();

    let mut x = Wrt::zeros(n);
    for i in 0..n {
        x.v[i] = f0[i].ln();
        x.v[n + i] = gain[i];
        x.v[2 * n + i] = q_to_bw(q[i]);
    }
    x.v[3 * n] = *amp;

    let mut g = Wrt::zeros(n);
    let mut best = Wrt::zeros(n);
    let mut best_l = 1e9f32;
    let mut sc = Scratch::new(n);

    let c = Consts {
        types,
        phi: &phi,
        r,
        n,
    };
    let mut opt = AdaBelief::new(3 * n + 1);

    for _ in 0..steps {
        let l = grad(&c, &x, &mut g, &mut sc);
        opt.step(&mut x, &g);

        // Box constraints via projection; zero that param's momentum on clamp.
        for i in 0..n {
            if limit(&mut x.v[i], lf_lim[i].lo, lf_lim[i].hi) {
                opt.m[i] = 0.0;
            }
            if limit(&mut x.v[n + i], gain_lim[i].lo, gain_lim[i].hi) {
                opt.m[n + i] = 0.0;
            }
            if limit(&mut x.v[2 * n + i], bw_lim[i].lo, bw_lim[i].hi) {
                opt.m[2 * n + i] = 0.0;
            }
        }

        if l < best_l {
            best_l = l;
            best.v.copy_from_slice(&x.v);
        }
    }

    for i in 0..n {
        f0[i] = best.v[i].exp();
        gain[i] = best.v[n + i];
        q[i] = bw_to_q(best.v[2 * n + i]);
    }
    *amp = best.v[3 * n];
    best_l
}

#[allow(clippy::too_many_arguments)]
fn autoeq_fit(
    steps: usize,
    types: &[Type],
    f0: &mut [f32],
    gain: &mut [f32],
    q: &mut [f32],
    amp: &mut f32,
    f0_lim: &[Lim],
    gain_lim: &[Lim],
    q_lim: &[Lim],
    n: usize,
    f: &[f32],
    r: &[f32],
) {
    let mut r_init = r.to_vec();
    for i in 0..n {
        let t = types[i];
        let p = init(t, &r_init, f, f0_lim[i], gain_lim[i], q_lim[i]);
        spectrum(t, p.f0, -p.gain, p.q, f, &mut r_init); // subtract from residual
        f0[i] = p.f0;
        gain[i] = p.gain;
        q[i] = p.q;
    }
    *amp = 0.0;
    fit(
        steps, types, f0, gain, q, amp, f0_lim, gain_lim, q_lim, n, f, r,
    );
}

// ── Preprocess: adaptive smoothing + treble roll-off ────────────────────────

struct Smooth {
    f0: f32,
    f1: f32,
    lo: f32,
    hi: f32,
    bias_f0: f32,
    bias_f1: f32,
    bias_f2: f32,
    bias_f3: f32,
    bias_lo: f32,
    bias_md: f32,
    bias_hi: f32,
    clip_f: f32,
}

const IE_SMOOTH: Smooth = Smooth {
    lo: 0.3,
    hi: 0.03,
    f0: 3000.0,
    f1: 12000.0,
    bias_lo: 0.0,
    bias_md: 0.15,
    bias_hi: 0.03,
    bias_f0: 10000.0,
    bias_f1: 13000.0,
    bias_f2: 14000.0,
    bias_f3: 20000.0,
    clip_f: 18500.0,
};

const OE_SMOOTH: Smooth = Smooth {
    lo: 0.3,
    hi: 0.03,
    f0: 5000.0,
    f1: 15000.0,
    bias_lo: 0.0,
    bias_md: 0.3,
    bias_hi: 0.2,
    bias_f0: 6000.0,
    bias_f1: 9000.0,
    bias_f2: 9000.0,
    bias_f3: 20000.0,
    clip_f: 17000.0,
};

fn search(x: &[f32], v: f32) -> usize {
    let mut idx = 0;
    let mut best = 1e9f32;
    for (i, &xi) in x.iter().enumerate() {
        let d = (xi - v).abs();
        if d < best {
            best = d;
            idx = i;
        }
    }
    idx
}

fn sgm(x: f32, x0: f32, x1: f32) -> f32 {
    let k = 4.0 / (x1 - x0);
    let m = 0.5 * (x0 + x1);
    let y = k * (x - m);
    0.5 * (0.5 * y).tanh() + 0.5
}

fn adaptive_smooth(s: &Smooth, f: &[f32], r: &mut [f32]) {
    const H: i32 = 48;
    let smooth_l0 = s.f0.ln();
    let smooth_l1 = s.f1.ln();
    let bias_l0 = s.bias_f0.ln();
    let bias_l1 = s.bias_f1.ln();
    let bias_l2 = s.bias_f2.ln();
    let bias_l3 = s.bias_f3.ln();

    let x = r.to_vec();
    let clip_idx = search(f, s.clip_f) as i32;

    for k in 0..K {
        let l = f[k].ln();
        let x_k = x[k];
        let sigma = s.lo + (s.hi - s.lo) * sgm(l, smooth_l0, smooth_l1);
        let bias = s.bias_lo
            + (s.bias_md - s.bias_lo) * sgm(l, bias_l0, bias_l1)
            + (s.bias_hi - s.bias_md) * sgm(l, bias_l2, bias_l3);

        let (mut a, mut c) = (0.0f32, 0.0f32);
        for j in -H..=H {
            let idx = (k as i32 + j).clamp(0, clip_idx) as usize;
            let x_s = x[idx];
            let d_spatial = sq(j as f32 * sigma);
            let d_range = bias * (x_s - x_k);
            let w = (-0.5 * d_spatial + d_range).exp();
            a += w * x_s;
            c += w;
        }
        r[k] = a / c;
    }
}

fn center_mean(x: &mut [f32]) -> f32 {
    let mean = x.iter().sum::<f32>() / x.len() as f32;
    for v in x.iter_mut() {
        *v -= mean;
    }
    mean
}

fn treble_rolloff(f: &[f32], r: &mut [f32], f_treble: f32) {
    let treble_idx = search(f, f_treble);
    let n_treble = K - treble_idx;
    if n_treble <= 1 {
        return;
    }
    let inv = 1.0 / (n_treble as f32 - 1.0);
    for i in 0..n_treble {
        let t = i as f32 * inv;
        r[treble_idx + i] *= (0.5 * PI * t).cos();
    }
}

/// Build the residual `target − smoothed(measured)`, demeaned, treble rolled off.
fn preprocess(f: &[f32], dst: &[f32], src: &[f32], smooth: Option<&Smooth>) -> Vec<f32> {
    let mut b = src.to_vec();
    if let Some(s) = smooth {
        adaptive_smooth(s, f, &mut b);
    }
    let mut r: Vec<f32> = (0..K).map(|k| dst[k] - b[k]).collect();
    center_mean(&mut r);
    let f_treble = if smooth.is_some() { 16000.0 } else { 18500.0 };
    treble_rolloff(f, &mut r, f_treble);
    r
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Fit `n_filters` biquads (a low shelf, a high shelf, then peaking filters) so
/// that `measured_db` EQ'd by them matches `target_db`. Both inputs are dB on
/// the [`log_freqs`] grid (length [`K`]). `steps` is the optimizer iteration
/// count (peqdb's default is 3000).
///
/// # Panics
///
/// Panics if `target_db` or `measured_db` is not exactly [`K`] samples long
/// (both must already be resampled onto the log-frequency grid).
pub fn run(
    target_db: &[f32],
    measured_db: &[f32],
    n_filters: usize,
    smoothing: Smoothing,
    steps: usize,
) -> AutoEqResult {
    assert_eq!(target_db.len(), K, "target must be on the K-point grid");
    assert_eq!(
        measured_db.len(),
        K,
        "measurement must be on the K-point grid"
    );
    let n = n_filters.clamp(1, MAX_N);

    let f = log_freqs();
    let smooth = match smoothing {
        Smoothing::InEar => Some(&IE_SMOOTH),
        Smoothing::OverEar => Some(&OE_SMOOTH),
        Smoothing::None => None,
    };
    let r = preprocess(&f, target_db, measured_db, smooth);

    // Filter plan: low shelf, high shelf, then peaks (peqdb's default config).
    let types: Vec<Type> = (0..n)
        .map(|i| match i {
            0 => Type::Lsc,
            1 => Type::Hsc,
            _ => Type::Pk,
        })
        .collect();
    let f0_lim = vec![
        Lim {
            lo: 20.0,
            hi: 16000.0
        };
        n
    ];
    let gain_lim = vec![
        Lim {
            lo: -16.0,
            hi: 16.0
        };
        n
    ];
    let q_lim: Vec<Lim> = (0..n)
        .map(|i| {
            if i < 2 {
                Lim { lo: 0.4, hi: 3.0 } // shelves
            } else {
                Lim { lo: 0.4, hi: 4.0 } // peaks
            }
        })
        .collect();

    let mut f0 = vec![0.0f32; n];
    let mut gain = vec![0.0f32; n];
    let mut q = vec![0.0f32; n];
    let mut amp = 0.0f32;
    autoeq_fit(
        steps, &types, &mut f0, &mut gain, &mut q, &mut amp, &f0_lim, &gain_lim, &q_lim, n, &f, &r,
    );

    // Headroom preamp: drop level so the loudest boosted point sits at 0 dB.
    let mut y = vec![0.0f32; K];
    for i in 0..n {
        spectrum(types[i], f0[i], gain[i], q[i], &f, &mut y);
    }
    let max = y.iter().copied().fold(f32::MIN, f32::max).max(0.0);

    let filters = (0..n)
        .map(|i| EqFilter {
            kind: match types[i] {
                Type::Pk => BandKind::Peak,
                Type::Lsc => BandKind::LowShelf,
                Type::Hsc => BandKind::HighShelf,
            },
            freq: f64::from(f0[i]),
            gain_db: f64::from(gain[i]),
            q: f64::from(q[i]),
        })
        .filter(|f| f.gain_db.abs() >= 0.1) // drop inaudible bands
        .collect();

    AutoEqResult {
        preamp_db: -f64::from(max),
        filters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_grid_spans_audible_band() {
        let f = log_freqs();
        assert_eq!(f.len(), K);
        assert!((f[0] - 20.0).abs() < 0.01);
        assert!((f[K - 1] - 20000.0).abs() < 1.0);
    }

    #[test]
    fn corrects_a_bump_toward_a_flat_target() {
        // Measurement = flat with a +8 dB peaking bump at ~3 kHz; target = flat.
        // AutoEQ should fit a corrective cut so the post-EQ error shrinks.
        let f = log_freqs();
        let target = vec![0.0f32; K];
        let mut measured = vec![0.0f32; K];
        spectrum(Type::Pk, 3000.0, 8.0, 2.0, &f, &mut measured);

        let res = run(&target, &measured, 6, Smoothing::None, 1200);
        assert!(!res.filters.is_empty(), "should fit at least one band");

        // Apply the fit to the measurement and check error drops vs no EQ.
        let mut eq = vec![0.0f32; K];
        for fl in &res.filters {
            let t = match fl.kind {
                BandKind::Peak => Type::Pk,
                BandKind::LowShelf => Type::Lsc,
                BandKind::HighShelf => Type::Hsc,
            };
            spectrum(
                t,
                fl.freq as f32,
                fl.gain_db as f32,
                fl.q as f32,
                &f,
                &mut eq,
            );
        }
        let before: f32 = (0..K).map(|k| sq(measured[k] - target[k])).sum();
        let after: f32 = (0..K)
            .map(|k| sq(measured[k] + eq[k] + res.preamp_db as f32 - target[k]))
            .sum();
        assert!(
            after < before * 0.5,
            "EQ should at least halve the squared error: before {before:.1} after {after:.1}"
        );
    }
}
