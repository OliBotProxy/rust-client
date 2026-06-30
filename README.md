# tunnel-client

The open-source client binary for [oli.bot](https://oli.bot) — expose any local HTTP server to the internet through a secure reverse-proxy tunnel, with no port forwarding required.

Works on Linux, macOS, Windows, and (with `--no-tls`) ESP32 / constrained embedded devices.

## Quick start

```bash
tunnel-client \
  --api-url  https://api-us.oli.bot/api \
  --tunnel-id <YOUR_TUNNEL_ID> \
  --api-key   <YOUR_API_KEY>
```

Get your tunnel ID and API key from [oli.bot](https://oli.bot) after signing up.

## Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--api-url` | — | API endpoint (`api-us`, `api-eu`, or `api-asia`) |
| `--tunnel-id` | — | Tunnel ID from the dashboard |
| `--api-key` | — | `<subscriptionId>_<salt>` API key |
| `--no-tls` | off | Plain TCP — for ESP32 or local testing |
| `--tls-server-name` | derived | Override TLS SNI hostname |
| `--tls-ca-cert-path` | — | Custom CA cert for the tunnel TLS connection |
| `--reconnect-interval` | 1 | Seconds between reconnect attempts |
| `--verbose` / `-v` | off | Debug logging |

## Regions

| Region | API URL |
|--------|---------|
| United States | `https://api-us.oli.bot/api` |
| Europe | `https://api-eu.oli.bot/api` |
| Asia | `https://api-asia.oli.bot/api` |

## Installation

### Linux (Debian / Ubuntu)

```bash
VER=$(curl -s https://api.github.com/repos/OliBotProxy/rust-client/releases/latest \
  | grep '"tag_name"' | cut -d'"' -f4 | sed 's/v//')
curl -LO "https://github.com/OliBotProxy/rust-client/releases/download/v${VER}/tunnel-client_${VER}_amd64.deb"
sudo dpkg -i "tunnel-client_${VER}_amd64.deb"
```

For arm64: replace `amd64` with `arm64` in the filename.

The package installs a systemd service. Edit `/etc/tunnel-client/env` with your credentials, then `sudo systemctl start tunnel-client`.

### Linux (RHEL / Rocky / Amazon Linux)

```bash
VER=$(curl -s https://api.github.com/repos/OliBotProxy/rust-client/releases/latest \
  | grep '"tag_name"' | cut -d'"' -f4 | sed 's/v//')
curl -LO "https://github.com/OliBotProxy/rust-client/releases/download/v${VER}/tunnel-client-${VER}-1.amd64.rpm"
sudo rpm -i "tunnel-client-${VER}-1.amd64.rpm"
```

For arm64: replace `amd64` with `arm64` in the filename.

### Windows

Download `tunnel-client-windows-<version>.zip` from [Releases](https://github.com/OliBotProxy/rust-client/releases), extract, and run `install.ps1` as Administrator. See [INSTALLATION.md](INSTALLATION.md) for details.

### Build from source

```bash
cargo build --bin tunnel-client --release
```

**Cross-compile for Linux musl (Apple Silicon):**

```bash
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
cargo build --bin tunnel-client --target x86_64-unknown-linux-musl --release
```

**Cross-compile for Windows:**

```bash
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
./scripts/build-windows.sh
```

## Protocol

Uses the oli.bot tunnel protocol v2 — a binary multiplexed framing protocol over TLS (or plain TCP with `--no-tls`). Frame header: 10 bytes (`type | stream_id | flags | length`, big-endian). See [tunnel-protocol.md](https://github.com/OliBotProxy/rust-client/blob/main/docs/tunnel-protocol.md) for the full spec.

## License

MIT
