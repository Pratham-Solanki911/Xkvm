# KVM-RS - Cross-Platform KVM + File/Clipboard Share

A cross-platform tool that shares keyboard & mouse, files, and clipboard between two computers over LAN, with TLS-encrypted, PIN-paired connections and a Tauri-based GUI panel.

## Features

- **Input Forwarding**: Share keyboard and mouse between computers. Deltas are captured on one rdev thread and broadcast to every paired client; the cursor warps back from screen edges so it never gets stuck against a monitor boundary.
- **File Transfer**: Resumable, SHA-256-verified transfer over its own TLS-protected port. Interrupted transfers resume from a `.part` file; a same-named file already at the destination is never overwritten (`name (1).ext`, `name (2).ext`, ...).
- **Clipboard Sync**: Bidirectional clipboard mirroring (text only, capped at 1 MiB), pollable on both ends.
- **Auto-Discovery**: Find servers via mDNS, or connect directly by address.
- **Secure by default**: TLS 1.3 on both the control and file-transfer ports, TOFU certificate pinning, and PIN-based device pairing — see [Security](#security).
- **GUI**: A Tauri + React panel (`ui/`) wraps the client library: discover/connect, drag-and-drop file send, known-servers list, live status.
- **Cross-Platform**: Windows and Linux are supported and tested in CI. macOS compiles from the same `cfg`-gated code paths but is untested (no CI runner currently exercises input capture/injection there) — see [Platform notes](#platform-specific-notes).

## Architecture

The project is a Cargo workspace with four crates:

- **`kvm-common`**: Shared wire protocol (`Envelope`/`Event`), pairing-proof helpers, framed async read/write, display-geometry helpers.
- **`kvm-server`** (`server/`): Server binary + library. Captures input, manages pairing/registry/TLS/SOCKS5, serves file transfers.
- **`kvm-client`** (`client/`): Client binary + library (`kvm_client`). The library (`config`, `discovery`, `file_transfer`, `inject`, `session`, `tls`, `addr`) is consumed by both the CLI binary and the `ui` crate.
- **`ui`**: Tauri desktop app built on `kvm-client`, with a React frontend.

## Building from Source

### Prerequisites

- Rust (stable; see `rust-toolchain.toml`) from [rustup.rs](https://rustup.rs)
- For Linux: `libx11-dev libxi-dev libxtst-dev libxcb1-dev libudev-dev` (see [`docs/INSTALL.md`](docs/INSTALL.md) for distro package names)
- For the GUI: Node.js 20+ and the platform's Tauri build dependencies (WebView2 on Windows; on Linux, `libwebkit2gtk-4.0-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libsoup2.4-dev libjavascriptcoregtk-4.0-dev`)

### Build

```bash
# Build the CLI server + client
cargo build --release -p kvm-server -p kvm-client

# Build the GUI (from ui/)
cd ui && npm ci && npm run build && cd ..
cargo build --release -p ui
```

## Usage

### Server

```bash
# Start with mDNS discovery, a random pairing PIN printed at startup
./target/release/kvm-server

# Fixed PIN, custom port, bind to a specific interface
./target/release/kvm-server --pin 123456 --port 5000 --bind 0.0.0.0

# Disable mDNS
./target/release/kvm-server --no-mdns

# Use a specific config file (default: <config_dir>/kvm-rs/server.toml)
./target/release/kvm-server --config /path/to/server.toml

# List paired devices / revoke one by fingerprint
./target/release/kvm-server --list-paired
./target/release/kvm-server --revoke AB:CD:...

# Optional SOCKS5 relay (requires auth unless --socks5-allow-anonymous)
./target/release/kvm-server --socks5-port 1080 --socks5-user alice --socks5-pass secret

# Append audit events to a file
./target/release/kvm-server --audit-log /var/log/kvm-rs-audit.log

# Verbose logging
./target/release/kvm-server -v
```

Full flag list: `kvm-server --help`.

### Client

```bash
# Auto-discover and connect to the first server found
./target/release/kvm-client --discover

# Connect to a specific server, providing the pairing PIN on first connect
./target/release/kvm-client --server 192.168.1.100 --pin 123456

# host:port form, or bracketed IPv6
./target/release/kvm-client --server 192.168.1.100:5000
./target/release/kvm-client --server [fe80::1]:4000

# Re-pin a server's certificate (e.g. after a legitimate cert rotation)
./target/release/kvm-client --server 192.168.1.100 --trust-new-cert

# Pin to a specific fingerprint instead of TOFU
./target/release/kvm-client --server 192.168.1.100 --fingerprint AB:CD:...

# Send a file to an already-known server and exit
./target/release/kvm-client --server 192.168.1.100 --send-file ./photo.png

# Disable the exponential-backoff auto-reconnect loop
./target/release/kvm-client --server 192.168.1.100 --no-reconnect

# Verbose logging
./target/release/kvm-client -v
```

Full flag list: `kvm-client --help`. Note: `--socks5-proxy` only prints a reminder — the client does not route traffic through the server's SOCKS5 relay itself; point your OS or browser's SOCKS5 settings at `socks5://<server>:<socks5-port>` instead.

### GUI

Run the built `ui` binary, or for development run `npm run dev` in `ui/` (starts the Vite dev server on `localhost:1420`, per `ui/tauri.conf.json`'s `devPath`) and, in another terminal, `cargo run -p ui` to launch the Tauri shell against it — there is no `tauri-cli` dependency in this project, so a `npm run tauri dev` script does not exist. It discovers servers, connects with an optional PIN and "trust new certificate" checkbox, shows the pinned fingerprint and connection status, lists known servers with a Forget button, and accepts file drops to send to the connected server.

## Platform-Specific Notes

### Linux

- **Input is mirrored, not blocked.** rdev's grab/block APIs are Windows/macOS-only. On Linux, `kvm-server` uses `rdev::listen`, so your own keyboard/mouse input keeps reaching local applications on the server machine at the same time it is forwarded to the client. This is logged once at startup.
- **Wayland is not supported.** rdev's capture and injection both depend on X11 (via XTest); run an X11 session (or XWayland-only apps) for input capture/injection to work.
- Input device access requires either membership in the `input` group or the udev rule in [`scripts/99-kvm-rs.rules`](scripts/99-kvm-rs.rules) — see [`docs/INSTALL.md`](docs/INSTALL.md).

### Windows

- On Windows/macOS, `kvm-server` uses rdev's `unstable_grab` to actually block local input while forwarding is on (mouse movement is still let through locally so the cursor keeps tracking).
- The installer (`scripts/build-windows-installer.ps1`) produces a plain batch-script installer (`install.bat` / `uninstall.bat`), **not** an MSI/EXE wizard. It must be run from an elevated (Administrator) prompt — it checks for this and exits early otherwise — and adds firewall rules plus the install directory to the machine `PATH`.

### macOS

The same `cfg(target_os = "macos")` branches used on Windows (grab-based blocking, hotkey swallowing) are compiled and unit-tested for logic, but there is no macOS CI runner exercising real input capture/injection, and macOS Accessibility-permission prompts are not automated. Treat macOS as best-effort/untested rather than a supported target.

## Protocol

KVM-RS uses a custom binary protocol (bincode) with 4-byte big-endian length-prefixed framing, protocol version `0.2.0` (checked strictly — a version mismatch closes the connection with an `Error`, no reconnect).

- **Transport**: TLS over TCP on two ports — the control port (default 4000) and a separate file-transfer port (default 4001), both using the server's self-signed certificate.
- **Discovery**: mDNS service type `_kvm-rs._tcp.local.`.

### Message Flow (control channel)

```
Client                                   Server
  |--- Hello{name, version, caps} ------->|
  |<-- HelloAck{file_transfer_port,       |
  |            server_name} --------------|
  |                                       |
  |--- PairRequest{nonce, fingerprint,    |
  |               proof} ----------------->|   proof = SHA-256(pin || 0 || nonce || 0 || fingerprint)
  |<-- PairAccept | PairReject{reason} ----|
  |                                       |
  |<-- ToggleForward(bool) ---------------|   (also sendable client -> server)
  |<-- Input(Event) -----------------------|   (continuous, while forwarding is on)
  |<-- Input(ReleaseAll) ------------------|   (sent whenever forwarding turns off)
  |<-- ClipboardSet{text} -----------------|   (either direction, as needed)
  |--- Ping(t) / <-- Pong(t) -------------->|   (keepalive, both directions)
  |<-- Goodbye ----------------------------|   (graceful shutdown)
```

The file-transfer port repeats a lightweight version of the same handshake: after the TLS handshake, the client sends `PairRequest` (any proof value; the server only checks that the fingerprint is already in `paired_devices` on this port), waits for `PairAccept`/`PairReject`, and only then sends `FileOffer{id, name, size, sha256}`. The server replies `FileAccept{id, start_offset}` (nonzero `start_offset` resumes a partial `.part` file) or `FileReject{id, reason}`, and confirms with `FileComplete{id}` once the hash verifies.

### Trust model

1. **Server identity**: a self-signed certificate generated on first run. Its fingerprint is `SHA-256(DER certificate)`, formatted as uppercase colon-separated hex (`AB:CD:...`), and printed at startup.
2. **Client identity**: a random 32-byte secret generated once and stored in `client.toml` (`client_secret`); the client's fingerprint sent in `PairRequest` is `SHA-256(client_secret)`. This fingerprint acts as a bearer token and is only ever sent inside an already-verified TLS connection to a pinned server.
3. **Pairing**: the server compares the presented `proof` against one computed from its own PIN; a match adds the fingerprint to its paired-devices list (persisted to `server.toml`) and replies `PairAccept`. An already-paired fingerprint is accepted without re-checking the proof. Repeated wrong-PIN attempts from the same peer IP (5 within 10 minutes) are rate-limited: that IP is rejected outright for the next 10 minutes.
4. **Certificate pinning (TOFU)**: the client pins the server's fingerprint on first connection to a given address and refuses to connect again if the fingerprint changes, unless `--trust-new-cert` (re-pins unconditionally) or `--fingerprint <hex>` (requires an exact match) is passed.

## Security

- **TLS on both ports**: the control connection (4000) and the file-transfer connection (4001) are each wrapped in TLS 1.3 using the server's self-signed certificate; the file port is not a plaintext side channel.
- **Trust-on-first-use certificate pinning**: see [Trust model](#trust-model) above. A fingerprint mismatch is refused by default — the client will not silently accept a different certificate at an address it has connected to before.
- **PIN-gated pairing**: new devices must present a PIN-derived proof before the server adds them to its paired-devices list; already-paired devices reconnect without re-entering the PIN. The server's PIN comes from (in order) `--pin`, the `KVM_PAIRING_PIN` environment variable, `server.toml`'s `pairing_pin`, or a random 6-digit PIN printed at startup.
- **Failed-pairing rate limiting**: 5 wrong proofs from one IP within 10 minutes blocks that IP for 10 minutes.
- **Audit logging**: pass `--audit-log <path>` (or set `audit_log` in `server.toml`) to append pairing, revocation, connection, and SOCKS5-startup events to a file, independent of the normal log stream.
- **SOCKS5 relay requires authentication by default**: if you enable the optional SOCKS5 proxy (`--socks5-port`), it refuses anonymous connections unless you explicitly pass `--socks5-allow-anonymous` (or set `socks5.allow_anonymous = true`). The password may also come from `KVM_SOCKS5_PASSWORD`.
- File names received over the file-transfer channel are sanitized (`Path::file_name()` only; empty/`.`/`..`/control-character names fall back to `received_file`), and destination files are never overwritten — a colliding name is written as `name (1).ext`, etc.

### Config and certificate location

- Linux: `~/.config/kvm-rs/`
- Windows: `%APPDATA%\kvm-rs\`
- macOS: `~/Library/Application Support/kvm-rs/`

## Configuration

Both `server.toml` and `client.toml` use `#[serde(default)]`, so a partial file (only the fields you care about) loads fine — anything missing falls back to its default.

### Server Config (`server.toml`)

```toml
server_name = "my-desktop"
auto_forward = false
clipboard_sync = true
transfer_dir = "/home/me/kvm-transfers"
file_transfer_port = 4001
pairing_pin = "123456"        # optional; falls back to --pin / KVM_PAIRING_PIN / a random PIN
bind_address = "0.0.0.0"
max_file_size = 0             # bytes; 0 = unlimited
audit_log = "/var/log/kvm-rs-audit.log"   # optional

[hotkey]
toggle_forward = "Ctrl+Alt+F"
show_panel = "Ctrl+Alt+K"

[socks5]
port = 1080
username = "alice"
password = "secret"
allow_anonymous = false

# Populated automatically as devices pair; fingerprint -> device info.
[paired_devices]
```

### Client Config (`client.toml`)

```toml
last_server = "192.168.1.100:4000"
auto_connect = false
client_secret = "..."   # generated automatically on first run; do not share

# Populated automatically as you connect to servers; address -> {name, fingerprint, last_connected}.
[known_servers]
```

## Hotkeys

`toggle_forward` and `show_panel` are parsed as `Modifier+Modifier+...+Key` (case-insensitive, whitespace-tolerant around `+`). Recognized modifiers: `Ctrl`/`Control`, `Alt`, `Shift`, `Super`/`Win`/`Meta`/`Cmd`. Recognized keys: `A`-`Z`, `0`-`9`, `F1`-`F12`, `Space`, `Escape`/`Esc`, `Tab`, `Enter`/`Return`, `Up`/`UpArrow`, `Down`/`DownArrow`, `Left`/`LeftArrow`, `Right`/`RightArrow`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Delete`/`Del`, `Backspace`, `Pause`, `ScrollLock`, `PrintScreen`/`PrtSc`, `` BackQuote``/`Grave`/`` ` ``.

The toggle-forward hotkey is detected inside the server's input-capture callback (it sees every key regardless of forwarding state), fires on key-down only (held-key repeats are ignored), and is debounced 250 ms. On Windows/macOS the chord's own key press/release is swallowed under input grab so it never reaches local applications; on Linux (where input isn't blocked at all — see [Platform-Specific Notes](#platform-specific-notes)) it is not swallowed either.

`show_panel` is reserved for a future GUI panel-focus shortcut and is currently unused by the server or the Tauri app.

## Firewall

The control port and the file-transfer port are both plain TCP and both need to be reachable from the client:

```powershell
# Windows, as Administrator (also done automatically by scripts/build-windows-installer.ps1)
netsh advfirewall firewall add rule name="KVM-RS Control TCP" dir=in action=allow protocol=TCP localport=4000
netsh advfirewall firewall add rule name="KVM-RS File Transfer TCP" dir=in action=allow protocol=TCP localport=4001
```

```bash
# Linux (ufw example)
sudo ufw allow 4000/tcp
sudo ufw allow 4001/tcp
```

If you enable the optional SOCKS5 relay, open its port too (default suggestion: 1080/tcp).

## Running the server as a service

See [`docs/INSTALL.md`](docs/INSTALL.md#systemd-service-linux-optional) — on Linux, `scripts/kvm-server.service` is a **user** systemd unit (`systemctl --user`), matching the fact that input capture/injection needs to run in the logged-in desktop session, not as a system-wide daemon.

## Development Roadmap

### Done
- [x] Framed, versioned control channel (bincode + length prefix)
- [x] Input capture (rdev) and injection (enigo), with a shared Linux/evdev-derived key code table
- [x] TLS 1.3 on both the control and file-transfer ports
- [x] TOFU certificate pinning with `--trust-new-cert` / `--fingerprint` overrides
- [x] PIN-based pairing with persisted paired-device list and IP rate limiting
- [x] Chunked, resumable, SHA-256-verified file transfer with collision-safe renaming
- [x] Bidirectional clipboard sync
- [x] mDNS discovery
- [x] Reconnect with exponential backoff + keepalive ping/pong on the client
- [x] Cross-platform hotkey handling (chord detection inside the capture callback, not OS-level registration)
- [x] Tauri + React GUI (discover, connect, drag-and-drop send, known servers, live status)
- [x] Audit logging
- [x] Optional authenticated SOCKS5 relay
- [x] Linux `.deb` packaging + user systemd unit; Windows batch installer
- [x] CI (fmt/clippy/test on Linux + Windows, plus a GUI build/check job)

### Not done / known gaps
- [ ] Multi-monitor-aware edge switching (today, forwarding is a single on/off toggle plus edge-warp cursor pinning, not automatic per-monitor-edge switching between machines)
- [ ] Clipboard image/rich-content support (text only, capped at 1 MiB)
- [ ] `--socks5-proxy` on the client only prints a reminder; it does not transparently route the client's own traffic
- [ ] macOS is untested (see [Platform-Specific Notes](#platform-specific-notes))
- [ ] No MSI/EXE installer wizard on Windows (batch-script installer only); no signed AppImage on Linux (an unsigned AppDir source tree is produced; finishing it into a `.AppImage` requires running `appimagetool` yourself)

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for build/test/lint commands and where configs and certificates live. Contributions are welcome via Pull Request.

## License

MIT License - see LICENSE file for details

## Acknowledgments

- [rdev](https://github.com/Narsil/rdev) for cross-platform input capture
- [enigo](https://github.com/enigo-rs/enigo) for cross-platform input injection
- [rustls](https://github.com/rustls/rustls) for TLS implementation
- [mdns-sd](https://github.com/keepsimple1/mdns-sd) for mDNS service discovery
- [tauri](https://tauri.app/) for the desktop GUI shell

## Troubleshooting

### Input capture not working on Linux

- Ensure you're in the `input` group: `groups | grep input`
- Check the udev rule is loaded: `ls -l /dev/uinput`
- Confirm you're running an X11 session, not Wayland (see [Platform-Specific Notes](#platform-specific-notes))

### Connection refused

- Check firewall settings on both machines (see [Firewall](#firewall))
- Verify the server is running: `netstat -an | grep 4000`
- Try connecting with an explicit address: `kvm-client --server <IP>[:port]`

### "server certificate fingerprint mismatch" on the client

The server's certificate fingerprint no longer matches what was pinned for this address on a previous connection — this is exactly what TOFU pinning is for, and it also happens if the server's certificate was legitimately regenerated (e.g. its key file was deleted). If you trust this new server, either:
- pass `--trust-new-cert` once to re-pin it, or
- pass `--fingerprint <the-server's-printed-fingerprint>` to pin an exact value explicitly.
Do not do this if you weren't expecting the server's certificate to change.

### `PairReject` / "pairing PIN required or incorrect"

The client's fingerprint isn't in the server's paired-device list and the PIN it presented didn't match. Pass the server's current PIN (printed at its startup, or set via `--pin`/`KVM_PAIRING_PIN`/`server.toml`) with `--pin` on the client, or `KVM_PAIRING_PIN`. If you see "too many failed pairing attempts", wait — the server rate-limits an IP for 10 minutes after 5 wrong attempts.

### Keys or mouse buttons "stuck" held down after a disconnect

The server sends `Input(ReleaseAll)` whenever forwarding turns off or a client disconnects, and the client releases every key/button it has injected (in reverse order) on receipt, on disconnect, and on error. If a key still appears stuck, check the client's log for injection errors around that time — it's the injector failing to release a specific key, not a protocol gap.

### High latency

- Check network connection (prefer wired over WiFi)
- Watch the client's logged `Latency` (from `Ping`/`Pong` round-trips, sent every 10s) to see whether the delay is on the network or elsewhere
- Close bandwidth-intensive applications

## FAQ

**Q: Does this work over the internet?**
A: KVM-RS is designed for LAN use (it has no NAT traversal / relay for internet-scale connections).

**Q: Can I use this with a VM?**
A: Yes, as long as the VM has network access to the host/server and its guest OS input driver doesn't itself intercept injected input.

**Q: Is there a GUI?**
A: Yes — `ui/` is a working Tauri + React app on top of the same client library the CLI uses.

**Q: Can I use this on Wayland?**
A: No — input capture/injection both go through X11/XTest via rdev/enigo. Run an X11 session.
