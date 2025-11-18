# KVM-RS - Cross-Platform KVM + File/Clipboard/Internet Share

A cross-platform tool that shares keyboard & mouse, files, clipboard, and optional internet between two computers over LAN (direct cable or network), with a collapsible UI panel and hotkey-based switching.

## Features

- **Input Forwarding**: Share keyboard and mouse between computers with low latency
- **File Transfer**: Drag & drop files with resume on interruption
- **Clipboard Sync**: Seamlessly copy/paste between machines
- **Internet Sharing** (planned): SOCKS5 proxy to route client traffic via server
- **Auto-Discovery**: Find servers automatically via mDNS or direct cable connection
- **Secure**: TLS encryption with pairing-based authentication
- **Cross-Platform**: Works on Linux and Windows (macOS support planned)

## Architecture

The project consists of three main components:

- **`kvm-common`**: Shared protocol definitions and serialization
- **`kvm-server`**: Server binary with input capture
- **`kvm-client`**: Client binary with input injection
- **`ui`** (planned): Tauri-based GUI with collapsible panel

## Building from Source

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- For Linux: `libudev-dev`, `libx11-dev`, `libxtst-dev`

### Build

```bash
# Build all components
cargo build --release

# Build server only
cargo build --release -p kvm-server

# Build client only
cargo build --release -p kvm-client
```

## Usage

### Server

```bash
# Start server with mDNS discovery
./target/release/kvm-server

# Start server on custom port
./target/release/kvm-server --port 5000

# Disable mDNS
./target/release/kvm-server --no-mdns

# Verbose logging
./target/release/kvm-server -v
```

### Client

```bash
# Auto-discover and connect to server
./target/release/kvm-client --discover

# Connect to specific server
./target/release/kvm-client --server 192.168.1.100

# Connect with custom port
./target/release/kvm-client --server 192.168.1.100 --port 5000

# Verbose logging
./target/release/kvm-client -v
```

## Platform-Specific Setup

### Linux

#### Input Capture & Injection

The server needs access to input devices, and the client needs to create virtual input devices.

**Option 1: Add user to input group (recommended)**

```bash
sudo usermod -a -G input $USER
# Log out and back in for changes to take effect
```

**Option 2: udev rules**

Create `/etc/udev/rules.d/99-kvm-rs.rules`:

```
KERNEL=="uinput", MODE="0660", GROUP="input"
```

Then reload udev:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

**Option 3: Run as root (not recommended)**

```bash
sudo ./target/release/kvm-server
sudo ./target/release/kvm-client
```

### Windows

#### Permissions

On Windows, input injection requires the application to run with appropriate permissions. The installer will handle this automatically.

For development builds:
- Run the command prompt as Administrator when testing

#### Firewall

You may need to allow the server through Windows Firewall:

```powershell
# Run as Administrator
netsh advfirewall firewall add rule name="KVM Server" dir=in action=allow protocol=TCP localport=4000
netsh advfirewall firewall add rule name="KVM File Transfer" dir=in action=allow protocol=TCP localport=4001
```

## Protocol

KVM-RS uses a custom binary protocol based on bincode serialization with length-prefixed framing:

1. **Framing**: Each message is prefixed with a 4-byte big-endian length field
2. **Serialization**: Messages are serialized using bincode
3. **Transport**: TLS over TCP (ports 4000 for control, 4001 for file transfer)
4. **Discovery**: mDNS service type `_kvm-rs._tcp.local.`

### Message Flow

```
Client                          Server
  |                               |
  |------- Hello ----------------->|
  |<------ HelloAck ---------------|
  |                               |
  |------- PairRequest ----------->|
  |<------ PairAccept -------------|
  |                               |
  |<------ Input(Event) -----------| (continuous)
  |<------ ClipboardSet -----------| (as needed)
  |                               |
```

## Security

- **TLS Encryption**: All communication is encrypted using TLS 1.3
- **Pairing**: First-time connections require explicit pairing
- **Self-Signed Certificates**: Server generates self-signed certificates on first run
- **Certificate Pinning**: Client remembers server fingerprints

### Certificate Location

- Linux: `~/.config/kvm-rs/`
- Windows: `%APPDATA%\kvm-rs\`
- macOS: `~/Library/Application Support/kvm-rs/`

## Configuration

Configuration files are stored in TOML format:

### Server Config

```toml
server_name = "my-desktop"
auto_forward = false
clipboard_sync = true

[hotkey]
toggle_forward = "Ctrl+Alt+F"
show_panel = "Ctrl+Alt+K"

[paired_devices]
# Automatically populated
```

### Client Config

```toml
last_server = "192.168.1.100:4000"
auto_connect = false

[known_servers]
# Automatically populated
```

## Development Roadmap

### M1 - Core MVP (CLI) ✅
- [x] Framed control channel
- [x] Input capture (rdev)
- [x] Input injection (enigo)
- [x] Explicit IP connect mode
- [x] TLS encryption
- [x] Pairing flow

### M2 - File Transfer & Clipboard
- [ ] Chunked resumable transfer
- [ ] Clipboard sync
- [ ] SHA256 integrity checks

### M3 - Discovery & Pairing
- [x] mDNS announce & browse
- [x] Basic pairing
- [ ] Persistent pairing storage
- [ ] Link-local fallback

### M4 - UI & UX
- [ ] Tauri-based panel
- [ ] Drag & drop
- [ ] Settings UI
- [ ] Transfer progress

### M5 - Packaging & CI
- [ ] Windows installer (MSI/EXE)
- [ ] Linux AppImage/deb
- [ ] macOS .app/dmg
- [ ] GitHub Actions CI

### M6 - Security & Polish
- [ ] Certificate management UI
- [ ] Device revocation
- [ ] Audit logs
- [ ] udev/systemd helpers

### M7 - Advanced Features
- [ ] SOCKS5 internet sharing
- [ ] QoS and latency optimization
- [ ] Multi-monitor support
- [ ] Clipboard image support

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

MIT License - see LICENSE file for details

## Acknowledgments

- [rdev](https://github.com/Narsil/rdev) for cross-platform input capture
- [enigo](https://github.com/enigo-rs/enigo) for cross-platform input injection
- [rustls](https://github.com/rustls/rustls) for TLS implementation
- [mdns-sd](https://github.com/keepsimple1/mdns-sd) for mDNS service discovery

## Troubleshooting

### Input capture not working on Linux

- Ensure you're in the `input` group: `groups | grep input`
- Check udev rules are loaded: `ls -l /dev/uinput`
- Try running with sudo (not recommended for production)

### Connection refused

- Check firewall settings on both machines
- Verify server is running: `netstat -an | grep 4000`
- Try connecting with explicit IP: `kvm-client --server <IP>`

### TLS handshake failed

- Ensure clocks are synchronized between machines
- Delete certificates and regenerate: `rm -rf ~/.config/kvm-rs/`

### High latency

- Check network connection (prefer wired over WiFi)
- Disable QoS or bandwidth limiting on router
- Close bandwidth-intensive applications

## FAQ

**Q: Does this work over the internet?**
A: Currently, KVM-RS is designed for LAN use. Internet support would require additional security measures and NAT traversal.

**Q: Can I use this with a VM?**
A: Yes, but you'll need to ensure the VM has network access to the host/server.

**Q: Is there a GUI?**
A: GUI is planned for M4. Current version is CLI-only.

**Q: How is this different from Synergy/Barrier?**
A: KVM-RS is written in Rust with a focus on security, low latency, and modern features like file transfer and clipboard sync. It's also fully open-source and actively developed.

**Q: Can I use this on Wayland?**
A: Input injection on Wayland has limitations due to security restrictions. X11 is recommended for full functionality.
