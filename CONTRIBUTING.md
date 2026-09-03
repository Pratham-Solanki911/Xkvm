# Contributing to KVM-RS

Thank you for your interest in contributing to KVM-RS! This document provides guidelines and instructions for setting up your development environment and contributing to the project.

## Development Environment Setup

### Linux (Ubuntu/Debian)

Install required dependencies:

```bash
# Build essentials
sudo apt-get update
sudo apt-get install -y build-essential pkg-config

# X11 development libraries
sudo apt-get install -y \
    libx11-dev \
    libxi-dev \
    libxtst-dev \
    libxcb1-dev \
    libxcb-render0-dev \
    libxcb-shape0-dev \
    libxcb-xfixes0-dev

# udev (for input device access)
sudo apt-get install -y libudev-dev

# Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Linux (Fedora/RHEL)

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

### Linux (Arch)

```bash
sudo pacman -S base-devel libx11 libxi libxtst libxcb systemd
```

### Windows

1. Install [Visual Studio 2022](https://visualstudio.microsoft.com/) with C++ development tools
2. Install [Rust](https://rustup.rs/)

No additional dependencies needed for Windows.

### macOS

```bash
# Install Xcode Command Line Tools
xcode-select --install

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Building the Project

```bash
# Clone the repository
git clone https://github.com/your-username/kvm-rs.git
cd kvm-rs

# Build all components
cargo build

# Build in release mode (optimized)
cargo build --release

# Run tests
cargo test

# Check code without building
cargo check
```

### Using a private build directory

If you are working alongside other agents/contributors on the same checkout
(or just want to avoid lock contention between parallel `cargo` invocations),
point `CARGO_TARGET_DIR` at a directory scoped to your own work instead of
sharing the workspace-wide `target/`:

```bash
CARGO_TARGET_DIR=target/my-area cargo build -p kvm-server
CARGO_TARGET_DIR=target/my-area cargo test -p kvm-server
```

This keeps your build artifacts (and the exclusive lock `cargo` takes on
`CARGO_TARGET_DIR`) separate from anyone else's, at the cost of a slower
first build in the new directory.

### Where configs and certificates live

At runtime, the server and client each load a TOML config from the OS config
directory (via the `dirs` crate) unless `--config <path>` is given:

- Server: `<config_dir>/kvm-rs/server.toml` (created with defaults on first
  run if missing).
- Client: `<config_dir>/kvm-rs/client.toml` (same behavior).

`<config_dir>` is `%APPDATA%` on Windows, `~/Library/Application Support` on
macOS, and `~/.config` on Linux (XDG). The server's TLS certificate and key
(`server.crt` / `server.key`) are generated alongside its config on first run
and are never checked into the repository — delete them to force
regeneration (this changes the server's fingerprint, so paired clients will
need to re-trust it).

## Running the Project

### Server

```bash
# Development build
cargo run -p kvm-server -- --verbose

# Release build
cargo run -p kvm-server --release
```

### Client

```bash
# Development build
cargo run -p kvm-client -- --discover --verbose

# Release build
cargo run -p kvm-client --release -- --server 192.168.1.100
```

## Code Style and Guidelines

### Rust Style

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `rustfmt` for code formatting. Check first, then apply:
  ```bash
  cargo fmt --all --check
  cargo fmt --all
  ```
- Use `clippy` for linting, matching what CI runs:
  ```bash
  cargo clippy --workspace --exclude ui --all-targets -- -D warnings
  ```
- Run the test suite:
  ```bash
  cargo test --workspace --exclude ui
  ```
- The `ui` crate (Tauri) is excluded from the commands above because it
  needs its frontend built first. Build and check it separately:
  ```bash
  cd ui
  npm ci
  npm run build
  cd ..
  cargo check -p ui
  cargo clippy -p ui -- -D warnings
  cargo fmt -p ui --check
  ```

### Commit Messages

Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification:

- `feat:` - New features
- `fix:` - Bug fixes
- `docs:` - Documentation changes
- `style:` - Code style changes (formatting, etc.)
- `refactor:` - Code refactoring
- `perf:` - Performance improvements
- `test:` - Test additions or modifications
- `chore:` - Build process or auxiliary tool changes

Examples:
```
feat: add clipboard image support
fix: resolve mouse capture issue on Wayland
docs: update installation instructions for Arch Linux
```

## Testing

### Unit Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p kvm-common

# Run tests with output
cargo test -- --nocapture
```

### Integration Tests

```bash
# Run integration tests
cargo test --test integration_test
```

### Manual Testing

For testing the full system:

1. Start the server on one machine (or terminal):
   ```bash
   cargo run -p kvm-server -- --verbose
   ```

2. Start the client on another machine (or terminal):
   ```bash
   cargo run -p kvm-client -- --discover --verbose
   ```

3. Test the following scenarios:
   - Input forwarding (keyboard and mouse)
   - File transfer
   - Clipboard sync
   - Connection interruption and recovery
   - Pairing flow

## Project Structure

```
kvm-rs/
├── common/           # Shared protocol definitions
│   ├── src/
│   │   └── lib.rs    # Protocol enums and serialization
│   └── Cargo.toml
├── server/           # Server binary
│   ├── src/
│   │   ├── main.rs         # Entry point
│   │   ├── capture.rs      # Input capture (rdev)
│   │   ├── server.rs       # TCP server
│   │   ├── tls.rs          # TLS certificate management
│   │   ├── discovery.rs    # mDNS service
│   │   ├── config.rs       # Configuration
│   │   └── file_transfer.rs # File transfer logic
│   └── Cargo.toml
├── client/           # Client binary
│   ├── src/
│   │   ├── main.rs         # Entry point
│   │   ├── inject.rs       # Input injection (enigo)
│   │   ├── discovery.rs    # mDNS client
│   │   └── config.rs       # Configuration
│   └── Cargo.toml
├── ui/               # Tauri GUI (future)
├── .github/
│   └── workflows/    # CI/CD workflows
├── Cargo.toml        # Workspace configuration
├── README.md
├── CONTRIBUTING.md
└── LICENSE
```

## Adding New Features

When adding a new feature:

1. **Create an issue** describing the feature
2. **Fork the repository** and create a new branch
3. **Implement the feature** with tests
4. **Update documentation** (README, comments, etc.)
5. **Run tests and linting**:
   ```bash
   cargo test
   cargo fmt
   cargo clippy -- -D warnings
   ```
6. **Submit a pull request** with a clear description

### Feature Checklist

- [ ] Code follows the project style guidelines
- [ ] Tests added for new functionality
- [ ] Documentation updated (README, code comments)
- [ ] No clippy warnings
- [ ] Code is formatted with rustfmt
- [ ] Commit messages follow Conventional Commits
- [ ] PR description clearly explains the changes

## Debugging

### Enable Debug Logging

```bash
# Set log level
export RUST_LOG=debug

# Run with debug logging
cargo run -p kvm-server
```

### Using a Debugger

#### VS Code

Create `.vscode/launch.json`:

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "lldb",
      "request": "launch",
      "name": "Debug Server",
      "cargo": {
        "args": ["build", "-p", "kvm-server"],
        "filter": {
          "name": "kvm-server",
          "kind": "bin"
        }
      },
      "args": ["--verbose"],
      "cwd": "${workspaceFolder}"
    }
  ]
}
```

#### Command Line (GDB)

```bash
# Build with debug symbols
cargo build -p kvm-server

# Run with gdb
gdb target/debug/kvm-server
```

## Performance Profiling

### CPU Profiling

```bash
# Install perf (Linux)
sudo apt-get install linux-tools-common linux-tools-generic

# Profile the server
cargo build --release -p kvm-server
perf record -g target/release/kvm-server

# Generate flamegraph
perf script | stackcollapse-perf.pl | flamegraph.pl > flame.svg
```

### Memory Profiling

```bash
# Install valgrind
sudo apt-get install valgrind

# Run with valgrind
cargo build -p kvm-server
valgrind --leak-check=full target/debug/kvm-server
```

## Common Issues

### Permission Denied (Input Devices)

**Problem**: Server can't capture input or client can't inject events

**Solution**:
```bash
# Add user to input group
sudo usermod -a -G input $USER

# Install udev rules
sudo cp scripts/99-kvm-rs.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger

# Log out and back in
```

### TLS Certificate Errors

**Problem**: TLS handshake fails

**Solution**:
```bash
# Remove old certificates
rm -rf ~/.config/kvm-rs/

# Regenerate on next run
```

### Port Already in Use

**Problem**: Server won't start, port 4000/4001 in use

**Solution**:
```bash
# Find process using the port
sudo lsof -i :4000

# Kill the process or use a different port
cargo run -p kvm-server -- --port 5000
```

## Getting Help

- **Issues**: [GitHub Issues](https://github.com/your-username/kvm-rs/issues)
- **Discussions**: [GitHub Discussions](https://github.com/your-username/kvm-rs/discussions)
- **Chat**: [Discord/Matrix Link]

## Code of Conduct

Please be respectful and professional in all interactions. We aim to maintain a welcoming and inclusive community.

## License

By contributing to KVM-RS, you agree that your contributions will be licensed under the MIT License.
