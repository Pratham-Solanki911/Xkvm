#!/bin/bash
set -e

echo "Building release binaries..."
cargo build --release -p kvm-server -p kvm-client

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PKG_NAME="kvm-rs"
# Single source of truth for the version: the workspace Cargo.toml.
VERSION="$(awk -F'"' '/^\[workspace.package\]/{f=1} f && /^version/{print $2; exit}' "$REPO_ROOT/Cargo.toml")"
if [ -z "$VERSION" ]; then
    echo "Error: could not read version from $REPO_ROOT/Cargo.toml"
    exit 1
fi
ARCH="amd64"
DEB_DIR="deb-package/${PKG_NAME}_${VERSION}_${ARCH}"

echo "Creating .deb package structure (version $VERSION)..."
rm -rf "$DEB_DIR"
mkdir -p "$DEB_DIR/DEBIAN"
mkdir -p "$DEB_DIR/usr/bin"
mkdir -p "$DEB_DIR/usr/lib/systemd/user"
mkdir -p "$DEB_DIR/etc/udev/rules.d"

cat <<EOF > "$DEB_DIR/DEBIAN/control"
Package: $PKG_NAME
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libc6, libx11-6, libxi6, libxtst6, libxcb1, libxcb-render0, libxcb-shape0, libxcb-xfixes0, systemd
Maintainer: KVM-RS Team
Description: Cross-platform KVM (keyboard/video/mouse) sharing tool
EOF

# Reload udev rules on install so the new rule takes effect without a reboot.
cat <<'EOF' > "$DEB_DIR/DEBIAN/postinst"
#!/bin/bash
set -e
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules && udevadm trigger
fi
exit 0
EOF
chmod 755 "$DEB_DIR/DEBIAN/postinst"

cp target/release/kvm-server "$DEB_DIR/usr/bin/"
cp target/release/kvm-client "$DEB_DIR/usr/bin/"
# kvm-server.service is a user unit (see scripts/kvm-server.service): it runs
# as the logged-in user, not as a system service, so it belongs under
# usr/lib/systemd/user/, not usr/lib/systemd/system/.
cp "$SCRIPT_DIR/kvm-server.service" "$DEB_DIR/usr/lib/systemd/user/"
cp "$SCRIPT_DIR/99-kvm-rs.rules" "$DEB_DIR/etc/udev/rules.d/"

echo "Building .deb package..."
dpkg-deb --build "$DEB_DIR"

echo "Creating AppDir structure for AppImage packaging..."
APPDIR="AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp target/release/kvm-server "$APPDIR/usr/bin/"
cp target/release/kvm-client "$APPDIR/usr/bin/"

cat <<EOF > "$APPDIR/AppRun"
#!/bin/bash
HERE="\$(dirname "\$(readlink -f "\${0}")")"
exec "\$HERE/usr/bin/kvm-client" "\$@"
EOF
chmod +x "$APPDIR/AppRun"

cat <<EOF > "$APPDIR/kvm-rs.desktop"
[Desktop Entry]
Type=Application
Name=KVM-RS
Comment=Cross-platform KVM (keyboard/video/mouse) sharing tool
Exec=kvm-client
Icon=kvm-rs
Categories=Utility;Network;
Terminal=true
EOF

# This AppDir is a real, runnable AppImage source tree (AppRun + .desktop),
# but it intentionally has no icon and is not turned into a .AppImage here:
# doing that requires the `appimagetool` binary (not available in this build
# environment). To finish packaging, run:
#   appimagetool AppDir kvm-rs-x86_64.AppImage
# after adding a kvm-rs.png/.svg icon next to kvm-rs.desktop.
echo "AppDir created at '$APPDIR'. Run 'appimagetool $APPDIR kvm-rs-x86_64.AppImage' to produce a real AppImage (not run here; appimagetool is not bundled)."

echo "Done! The .deb package is in deb-package/ and the AppImage source tree is in $APPDIR/."
