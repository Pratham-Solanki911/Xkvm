# Installation Guide

This guide covers installation of KVM-RS on different platforms. It documents what the build actually produces (see `.github/workflows/release.yml` and `scripts/`) — not aspirational package formats.

## Table of Contents

- [Linux](#linux)
  - [From a release archive](#linux-from-a-release-archive)
  - [From the `.deb` package](#linux-from-the-deb-package)
  - [From Source](#linux-from-source)
  - [systemd Service (Optional)](#linux-systemd-service-optional)
- [Windows](#windows)
  - [Batch installer](#windows-batch-installer)
  - [From a release archive](#windows-from-a-release-archive)
  - [From Source](#windows-from-source)
- [macOS](#macos)
  - [From Source](#macos-from-source)
- [GUI (Tauri app)](#gui-tauri-app)
- [Post-Installation Setup](#post-installation-setup)
- [Troubleshooting](#troubleshooting)
- [Uninstallation](#uninstallation)

## Linux

### Linux: From a release archive

Each tagged release publishes a `.tar.gz` per architecture containing the `kvm-server` and `kvm-client` binaries plus `scripts/` and `docs/`.

1. Download and extract the archive from the release's assets, then:
   ```bash
   sudo cp kvm-server kvm-client /usr/local/bin/
   sudo chmod +x /usr/local/bin/kvm-server /usr/local/bin/kvm-client
   ```

2. Install the udev rule (required for input device access):
   ```bash
   sudo ./scripts/install_udev_rules.sh
   ```

3. Add your user to the `input` group:
   ```bash
   sudo usermod -a -G input $USER
   ```

4. Log out and log back in for group changes to take effect.

### Linux: From the `.deb` package

`scripts/build-linux-package.sh` produces a `.deb` (in `deb-package/`) that installs the binaries, the udev rule, and the systemd **user** unit, and reloads udev rules on install via its `postinst` script:

```bash
sudo dpkg -i kvm-rs_<version>_amd64.deb
sudo apt-get install -f   # pull in any missing runtime libs
```

There is currently no RPM, AUR, or AppImage release artifact. `scripts/build-linux-package.sh` also generates an `AppDir/` source tree (`AppRun` + `.desktop`) as a starting point for an AppImage, but does not run `appimagetool` itself (it isn't bundled in the build environment) — see the script's own output for the exact command to finish that step yourself.

### Linux: From Source

1. Install dependencies:

   **Ubuntu/Debian:**
   ```bash
   sudo apt-get update
   sudo apt-get install -y \
       build-essential \
       pkg-config \
       libx11-dev \
       libxi-dev \
       libxtst-dev \
       libxcb1-dev \
       libudev-dev
   ```

   **Fedora/RHEL:**
   ```bash
   sudo dnf groupinstall "Development Tools"
   sudo dnf install -y \
       libX11-devel \
       libXi-devel \
       libXtst-devel \
       libxcb-devel \
       systemd-devel \
       pkg-config
   ```

   **Arch Linux:**
   ```bash
   sudo pacman -S base-devel libx11 libxi libxtst libxcb systemd
   ```

2. Install Rust:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```

3. Clone and build:
   ```bash
   git clone <this repository>
   cd Xkvm
   cargo build --release -p kvm-server -p kvm-client
   ```

4. Install binaries:
   ```bash
   sudo cp target/release/kvm-server target/release/kvm-client /usr/local/bin/
   ```

5. Install the udev rule:
   ```bash
   sudo ./scripts/install_udev_rules.sh
   sudo usermod -a -G input $USER
   ```

### Linux: systemd Service (Optional)

`scripts/kvm-server.service` is a **user** unit, not a system-wide one — the server needs to run inside your logged-in desktop session to reach your input devices and clipboard, so it is installed under `~/.config/systemd/user/` (or, via the `.deb`, `/usr/lib/systemd/user/`, which `systemctl --user` also reads) and managed with `systemctl --user`, never `sudo systemctl`.

1. If you built/installed from source rather than the `.deb`, copy the unit yourself:
   ```bash
   mkdir -p ~/.config/systemd/user
   cp scripts/kvm-server.service ~/.config/systemd/user/
   ```

2. Enable and start it for your own user session (no `sudo`, no `@` instance suffix):
   ```bash
   systemctl --user enable --now kvm-server
   ```

3. Check status / logs:
   ```bash
   systemctl --user status kvm-server
   journalctl --user -u kvm-server -f
   ```

## Windows

### Windows: Batch installer

`scripts/build-windows-installer.ps1` builds the release binaries and produces `installer-windows/install.bat` and `uninstall.bat`. This is a plain batch-script installer, **not** an MSI or wizard-style EXE.

1. Run `scripts\build-windows-installer.ps1` (or use a pre-built `installer-windows/` folder from a release archive).
2. Right-click `install.bat` and choose **Run as administrator** — it checks for an elevated session itself and exits immediately with a message if it isn't elevated.
3. It will:
   - Copy `kvm-server.exe`, `kvm-client.exe`, and `README.md` to `%ProgramFiles%\KVM-RS`
   - Add Windows Firewall rules for TCP 4000 and 4001
   - Create a Start Menu shortcut for `kvm-server.exe`
   - Add the install directory to the **machine** `PATH` (a new terminal is needed for this to take effect)
4. `uninstall.bat` (also must be run as Administrator) reverses all of the above.

### Windows: From a release archive

1. Download and extract the `.zip` from a release's assets.
2. Add the extracted folder to your `PATH` manually (System Properties → Environment Variables), or just invoke the binaries with a full path.
3. Configure the firewall (as Administrator):
   ```powershell
   netsh advfirewall firewall add rule name="KVM-RS Control TCP" dir=in action=allow protocol=TCP localport=4000
   netsh advfirewall firewall add rule name="KVM-RS File Transfer TCP" dir=in action=allow protocol=TCP localport=4001
   ```

### Windows: From Source

1. Install Visual Studio 2022 (or the Build Tools) with the "Desktop development with C++" workload.
2. Install Rust from [rustup.rs](https://rustup.rs/).
3. Clone and build:
   ```powershell
   git clone <this repository>
   cd Xkvm
   cargo build --release -p kvm-server -p kvm-client
   ```
4. Binaries will be in `target\release\`.

There is no Windows service wrapper shipped with this project; run `kvm-server.exe` directly, or manage it yourself with a tool like NSSM if you want it to run unattended.

## macOS

macOS is a compile target (the same `cfg(target_os = "macos")` code paths used on Windows for input-grab and hotkey handling apply here) but is **not covered by CI and is untested** — no DMG, `.app` bundle, or Accessibility-permission automation is provided.

### macOS: From Source

1. Install Xcode Command Line Tools:
   ```bash
   xcode-select --install
   ```
2. Install Rust:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```
3. Clone and build:
   ```bash
   git clone <this repository>
   cd Xkvm
   cargo build --release -p kvm-server -p kvm-client
   ```
4. Install:
   ```bash
   sudo cp target/release/kvm-server target/release/kvm-client /usr/local/bin/
   ```
5. Input injection on macOS requires granting Accessibility permission to the terminal (or binary) running `kvm-server`/`kvm-client`, under System Settings → Privacy & Security → Accessibility. This has not been exercised end-to-end by the project's own testing.

## GUI (Tauri app)

The `ui/` crate is a Tauri + React desktop app built on the `kvm-client` library. To build it:

```bash
cd ui
npm ci
npm run build
cargo build --release -p ui
```

`cargo build --release -p ui` (there is no separate `src-tauri/` subfolder — the `ui` crate at the workspace root *is* the Tauri backend) produces only the `ui` binary directly under `target/release/`. This project depends on `tauri`/`tauri-build` but not on `tauri-cli`, so nothing here invokes Tauri's bundler: no `target/release/bundle/` package is produced by this command. Packaging a `.deb`/AppImage/installer would require adding `tauri-cli` (or `@tauri-apps/cli`) and running `cargo tauri build` / `npm run tauri build` instead — that is not set up in this repository today. The `ui` binary itself has the same TLS/pairing/discovery behavior as the CLI client, wrapped in a GUI panel.

## Post-Installation Setup

### First Run

Run the server for the first time to generate its TLS certificate and a default config file:

```bash
kvm-server --verbose
```

This will:
1. Generate a self-signed TLS certificate under `~/.config/kvm-rs/` (Linux), `%APPDATA%\kvm-rs\` (Windows), or `~/Library/Application Support/kvm-rs/` (macOS)
2. Write a default `server.toml` next to it, if one doesn't exist yet
3. Print the server's certificate fingerprint and, unless a PIN was configured, a random pairing PIN
4. Start the mDNS service (unless `--no-mdns`) and listen for client connections

### Configuration

The default config file lives at:
- Linux/macOS: `~/.config/kvm-rs/server.toml` (client: `client.toml` in the same directory)
- Windows: `%APPDATA%\kvm-rs\server.toml` (client: `client.toml`)

Only the fields you want to override need to be present — see the [main README's Configuration section](../README.md#configuration) for the full field list and an example of each file.

### Verify Installation

1. Check version:
   ```bash
   kvm-server --version
   kvm-client --version
   ```

2. Start the server:
   ```bash
   kvm-server --verbose
   ```

3. From another machine, discover and connect (providing the PIN printed by the server):
   ```bash
   kvm-client --discover --pin <printed PIN>
   ```

## Troubleshooting

### Linux: Permission Denied accessing input devices

```bash
# Check if you're in the input group
groups | grep input

# If not, add yourself
sudo usermod -a -G input $USER

# Log out and back in, then verify
groups | grep input
```

### Windows: Firewall Blocking

If connections fail and you didn't use the batch installer (which adds these automatically):

1. Open Windows Defender Firewall
2. Click "Allow an app through firewall" → "Change settings" → "Allow another app"
3. Browse to `kvm-server.exe` and add it
4. Ensure both "Private" and "Public" are checked

### macOS: Accessibility Permission Denied

If input injection doesn't work:

1. System Settings → Privacy & Security → Accessibility
2. Click "+" and add the terminal app (or binary) you're running `kvm-server`/`kvm-client` from
3. Restart the process

### Certificate / pairing issues

See the main README's [Troubleshooting section](../README.md#troubleshooting) for "fingerprint mismatch" and "PairReject" specifically. To force fresh certificate generation on the server:

```bash
rm ~/.config/kvm-rs/server.key ~/.config/kvm-rs/server.crt   # Linux/macOS
# or
del %APPDATA%\kvm-rs\server.key %APPDATA%\kvm-rs\server.crt  # Windows
```
This changes the server's fingerprint, so every client that had it pinned will need `--trust-new-cert` (or the new `--fingerprint`) on its next connection.

## Uninstallation

### Linux

```bash
# Stop and remove the user service, if installed
systemctl --user disable --now kvm-server

# Remove binaries
sudo rm /usr/local/bin/kvm-server /usr/local/bin/kvm-client
# (or, if installed via .deb): sudo dpkg -r kvm-rs

# Remove udev rule
sudo rm /etc/udev/rules.d/99-kvm-rs.rules
sudo udevadm control --reload-rules

# Remove configuration and certificates
rm -rf ~/.config/kvm-rs/

# Remove from input group (optional)
sudo gpasswd -d $USER input
```

### Windows

Run `uninstall.bat` from wherever `install.bat` was run (as Administrator), or manually:

```powershell
Remove-Item -Recurse -Force "C:\Program Files\KVM-RS"
Remove-Item -Recurse -Force "$env:APPDATA\kvm-rs"

netsh advfirewall firewall delete rule name="KVM-RS Control TCP"
netsh advfirewall firewall delete rule name="KVM-RS File Transfer TCP"
```

### macOS

```bash
sudo rm /usr/local/bin/kvm-server /usr/local/bin/kvm-client
rm -rf ~/Library/Application\ Support/kvm-rs/
```
