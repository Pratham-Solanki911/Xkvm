# Installation Guide

This guide covers installation of KVM-RS on different platforms.

## Table of Contents

- [Linux](#linux)
  - [From Binary](#linux-from-binary)
  - [From Source](#linux-from-source)
  - [AppImage](#linux-appimage)
  - [Package Managers](#linux-package-managers)
- [Windows](#windows)
  - [Installer](#windows-installer)
  - [From Binary](#windows-from-binary)
  - [From Source](#windows-from-source)
- [macOS](#macos)
  - [From Binary](#macos-from-binary)
  - [From Source](#macos-from-source)
- [Post-Installation Setup](#post-installation-setup)

## Linux

### Linux: From Binary

1. Download the latest release for your architecture:
   ```bash
   wget https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs-linux-x86_64.tar.gz
   ```

2. Extract the archive:
   ```bash
   tar xzf kvm-rs-linux-x86_64.tar.gz
   cd kvm-rs-linux-x86_64
   ```

3. Install binaries:
   ```bash
   sudo cp kvm-server kvm-client /usr/local/bin/
   sudo chmod +x /usr/local/bin/kvm-server /usr/local/bin/kvm-client
   ```

4. Install udev rules (required for input device access):
   ```bash
   sudo ./install_udev_rules.sh
   ```

5. Add your user to the input group:
   ```bash
   sudo usermod -a -G input $USER
   ```

6. Log out and log back in for group changes to take effect.

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
   git clone https://github.com/your-org/kvm-rs.git
   cd kvm-rs
   cargo build --release
   ```

4. Install binaries:
   ```bash
   sudo cp target/release/kvm-server target/release/kvm-client /usr/local/bin/
   ```

5. Install udev rules:
   ```bash
   sudo ./scripts/install_udev_rules.sh
   sudo usermod -a -G input $USER
   ```

### Linux: AppImage

1. Download the AppImage:
   ```bash
   wget https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs-x86_64.AppImage
   ```

2. Make it executable:
   ```bash
   chmod +x kvm-rs-x86_64.AppImage
   ```

3. Run:
   ```bash
   ./kvm-rs-x86_64.AppImage --server  # or --client
   ```

### Linux: Package Managers

**Debian/Ubuntu (.deb):**
```bash
wget https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs_amd64.deb
sudo dpkg -i kvm-rs_amd64.deb
sudo apt-get install -f  # Install dependencies
```

**Fedora/RHEL (.rpm):**
```bash
wget https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs.x86_64.rpm
sudo rpm -i kvm-rs.x86_64.rpm
```

**Arch Linux (AUR):**
```bash
yay -S kvm-rs
# or
paru -S kvm-rs
```

### Linux: systemd Service (Optional)

To run the server as a system service:

1. Copy the service file:
   ```bash
   sudo cp scripts/kvm-server.service /etc/systemd/system/kvm-server@.service
   ```

2. Enable and start for your user:
   ```bash
   sudo systemctl enable kvm-server@$USER
   sudo systemctl start kvm-server@$USER
   ```

3. Check status:
   ```bash
   sudo systemctl status kvm-server@$USER
   ```

## Windows

### Windows: Installer

1. Download the installer:
   - [kvm-rs-setup-x64.exe](https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs-setup-x64.exe)

2. Run the installer as Administrator

3. Follow the installation wizard

4. The installer will:
   - Install binaries to `C:\Program Files\KVM-RS`
   - Add to PATH
   - Configure Windows Firewall
   - Create Start Menu shortcuts

### Windows: From Binary

1. Download the archive:
   ```powershell
   # Using PowerShell
   Invoke-WebRequest -Uri "https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs-windows-x86_64.zip" -OutFile "kvm-rs.zip"
   ```

2. Extract:
   ```powershell
   Expand-Archive -Path kvm-rs.zip -DestinationPath C:\kvm-rs
   ```

3. Add to PATH:
   - Open System Properties → Environment Variables
   - Add `C:\kvm-rs` to the System PATH

4. Configure firewall (as Administrator):
   ```powershell
   netsh advfirewall firewall add rule name="KVM Server" dir=in action=allow protocol=TCP localport=4000
   netsh advfirewall firewall add rule name="KVM File Transfer" dir=in action=allow protocol=TCP localport=4001
   ```

### Windows: From Source

1. Install Visual Studio 2022 with C++ development tools

2. Install Rust:
   - Download from [rustup.rs](https://rustup.rs/)
   - Run the installer

3. Clone and build:
   ```powershell
   git clone https://github.com/your-org/kvm-rs.git
   cd kvm-rs
   cargo build --release
   ```

4. Binaries will be in `target\release\`

### Windows: Service Installation (Optional)

To run as a Windows service:

```powershell
# As Administrator
sc create KVMServer binPath= "C:\Program Files\KVM-RS\kvm-server.exe" start= auto
sc start KVMServer
```

## macOS

### macOS: From Binary

1. Download the DMG:
   ```bash
   curl -L -O https://github.com/your-org/kvm-rs/releases/latest/download/kvm-rs-macos-x86_64.dmg
   ```

2. Mount and install:
   ```bash
   hdiutil attach kvm-rs-macos-x86_64.dmg
   cp -R /Volumes/KVM-RS/KVM-RS.app /Applications/
   hdiutil detach /Volumes/KVM-RS
   ```

3. Grant Accessibility permissions:
   - System Preferences → Security & Privacy → Privacy
   - Select "Accessibility"
   - Add KVM-RS and grant permission

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
   git clone https://github.com/your-org/kvm-rs.git
   cd kvm-rs
   cargo build --release
   ```

4. Install:
   ```bash
   sudo cp target/release/kvm-server target/release/kvm-client /usr/local/bin/
   ```

## Post-Installation Setup

### First Run

After installation, run the server for the first time to generate certificates:

```bash
kvm-server --verbose
```

This will:
1. Generate TLS certificates in `~/.config/kvm-rs/` (Linux/macOS) or `%APPDATA%\kvm-rs\` (Windows)
2. Start the mDNS service
3. Listen for client connections

### Configuration

Create a configuration file at:
- Linux/macOS: `~/.config/kvm-rs/config.toml`
- Windows: `%APPDATA%\kvm-rs\config.toml`

Example configuration:

```toml
server_name = "my-workstation"
auto_forward = false
clipboard_sync = true

[hotkey]
toggle_forward = "Ctrl+Alt+F"
show_panel = "Ctrl+Alt+K"
```

### Verify Installation

1. Check version:
   ```bash
   kvm-server --version
   kvm-client --version
   ```

2. Start server:
   ```bash
   kvm-server --verbose
   ```

3. From another machine, discover the server:
   ```bash
   kvm-client --discover
   ```

## Troubleshooting

### Linux: Permission Denied

If you get permission errors:

```bash
# Check if you're in the input group
groups | grep input

# If not, add yourself
sudo usermod -a -G input $USER

# Log out and back in, then verify
groups | grep input
```

### Windows: Firewall Blocking

If connections fail:

1. Open Windows Defender Firewall
2. Click "Allow an app through firewall"
3. Click "Change settings" → "Allow another app"
4. Browse to `kvm-server.exe` and add it
5. Ensure both "Private" and "Public" are checked

### macOS: Accessibility Permission Denied

If input injection doesn't work:

1. System Preferences → Security & Privacy → Privacy
2. Select "Accessibility" from the left sidebar
3. Click the lock to make changes
4. Click "+" and add `/usr/local/bin/kvm-server`
5. Restart the server

### Certificate Issues

If you get TLS errors:

```bash
# Remove old certificates
rm -rf ~/.config/kvm-rs/  # Linux/macOS
# or
del %APPDATA%\kvm-rs\*.crt %APPDATA%\kvm-rs\*.key  # Windows

# Certificates will be regenerated on next run
```

## Uninstallation

### Linux

```bash
# Remove binaries
sudo rm /usr/local/bin/kvm-server /usr/local/bin/kvm-client

# Remove udev rules
sudo rm /etc/udev/rules.d/99-kvm-rs.rules
sudo udevadm control --reload-rules

# Remove configuration
rm -rf ~/.config/kvm-rs/

# Remove from input group (optional)
sudo gpasswd -d $USER input
```

### Windows

Use "Add or Remove Programs" to uninstall, or manually:

```powershell
# Remove from PATH
# (via System Properties → Environment Variables)

# Remove files
Remove-Item -Recurse -Force "C:\Program Files\KVM-RS"
Remove-Item -Recurse -Force "$env:APPDATA\kvm-rs"

# Remove firewall rules
netsh advfirewall firewall delete rule name="KVM Server"
netsh advfirewall firewall delete rule name="KVM File Transfer"
```

### macOS

```bash
# Remove application
rm -rf /Applications/KVM-RS.app

# Remove binaries
sudo rm /usr/local/bin/kvm-server /usr/local/bin/kvm-client

# Remove configuration
rm -rf ~/.config/kvm-rs/
```

## Next Steps

- Read the [Quick Start Guide](QUICKSTART.md)
- Review the [User Manual](USER_MANUAL.md)
- Check out [Troubleshooting](TROUBLESHOOTING.md)
- Join the community on [Discord/GitHub Discussions]
