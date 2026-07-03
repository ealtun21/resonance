# Maintainer: ealtun21 <ealtun21@ku.edu.tr>
# 'resonance' is already taken on the AUR (an unrelated GTK4 music player),
# so this package is named 'resonance-eq'. The installed binaries keep their
# plain names (resonanced, resonance, resonance-tui, resonance-gui).
pkgname=resonance-eq
pkgver=0.9.0
pkgrel=1
pkgdesc="Terminal EQ daemon for Linux/PipeWire with FxSound .fac and EqualizerAPO preset support"
arch=('x86_64')
url="https://github.com/ealtun21/resonance"
license=('GPL-3.0-or-later')
# dbus: the resonance-tray StatusNotifier tray (ksni) links libdbus at build
# and talks to the session bus at runtime.
depends=('pipewire' 'libpipewire' 'dbus')
makedepends=('cargo' 'pkgconf')
provides=('resonanced')
conflicts=('resonance-eq-git')
# Build from the local checkout. For a tagged release source build use:
#   source=("$pkgname-$pkgver.tar.gz::$url/archive/refs/tags/v$pkgver.tar.gz")
options=('!lto')

prepare() {
    # When building in-tree (install.sh), the sources live in the repo root.
    cd "$startdir"
    export RUSTUP_TOOLCHAIN=stable
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
    cd "$startdir"
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --release --locked --all
}

check() {
    cd "$startdir"
    export RUSTUP_TOOLCHAIN=stable
    cargo test --release --locked --all
}

package() {
    cd "$startdir"
    install -Dm755 target/release/resonanced     "$pkgdir/usr/bin/resonanced"
    install -Dm755 target/release/resonance       "$pkgdir/usr/bin/resonance"
    install -Dm755 target/release/resonance-tui   "$pkgdir/usr/bin/resonance-tui"
    install -Dm755 target/release/resonance-gui   "$pkgdir/usr/bin/resonance-gui"
    install -Dm755 target/release/resonance-tray  "$pkgdir/usr/bin/resonance-tray"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"

    # Desktop integration for the GUI.
    local appid="io.github.ealtun21.Resonance"
    install -Dm644 "contrib/$appid.desktop" \
        "$pkgdir/usr/share/applications/$appid.desktop"
    install -Dm644 "contrib/$appid.svg" \
        "$pkgdir/usr/share/icons/hicolor/scalable/apps/$appid.svg"
    install -Dm644 "contrib/$appid.metainfo.xml" \
        "$pkgdir/usr/share/metainfo/$appid.metainfo.xml"

    # systemd user service.
    install -Dm644 contrib/systemd/resonanced.service \
        "$pkgdir/usr/lib/systemd/user/resonanced.service"
}
