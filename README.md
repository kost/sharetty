# sharetty

A modern, high-performance TTY sharing tool written in Rust, inspired by `tty2web`. `sharetty` allows you to share your terminal session over the web simply and securely, with support for advanced features like HTTP/3 (QUIC), TLS, and file transfers.

## Features

- **Web-based Terminal**: Access your terminal from any browser using xterm.js.
- **High Performance**: Built with `axum`, `tokio`, and `h3` for asynchronous I/O and low latency.
- **Secure**:
    - Full TLS/SSL support (HTTP/2 & HTTP/3).
    - Basic Authentication (`-c user:pass`).
    - Random URL generation (`-r`) or custom URL prefixes (`--url`).
- **Cross-Platform**:
    - **Unix**: Linux, macOS, BSD (via `pty-process`).
    - **Windows**: Windows 10/11, Server 2019+ (via `winpty-rs`).
- **Network Agnostic**:
    - IPv6 support.
    - Protocol options: HTTP/1.1 (default), HTTP/2 (`-2`), and HTTP/3 QUIC (`-3`).
- **File Transfer**:
    - Directory listing and download (`--download`).
    - File upload (`--upload`).
- **One-Shot Mode**: Exit after a single client disconnects (`--once`).
- **Read-Only**: Clients are read-only by default (enable write with `-w`).
- **Embedded Assets**: Single binary deployment (no external frontend files needed).

## Installation

### From Source

#### Unix (Linux/macOS)

```bash
git clone https://github.com/yourusername/sharetty.git
cd sharetty/sharetty
cargo build --release
```

#### Windows

Ensure you have Visual Studio Build Tools (Desktop development with C++) installed.

```cmd
git clone https://github.com/yourusername/sharetty.git
cd sharetty/sharetty
cargo build --release
```

The binary will be located at `target/release/sharetty`.

## Usage

### Unix

Basic usage to share a `bash` session on port 3000 (read-only):

```bash
./sharetty -p 3000 bash
```

### Windows

Basic usage to share a `cmd.exe` session:

```cmd
sharetty.exe -p 3000 cmd.exe
```

PowerShell:

```cmd
sharetty.exe -p 3000 powershell.exe
```

Allow clients to write (interact with the shell):

```bash
./sharetty -p 3000 -w bash
```

### Command Line Options

| Flag | Description |
|------|-------------|
| `-p, --port <PORT>` | Port to listen on (default: 3000) |
| `-a, --address <ADDR>` | Address to bind to (default: 0.0.0.0) |
| `-w, --permit-write` | Allow clients to write to the TTY |
| `-c, --credential <USER:PASS>` | Enable Basic Auth |
| `-r, --random-url` | Generate a random URL path |
| `--random-url-length <LEN>` | Length of random URL (default: 8) |
| `--url <PATH>` | Serve at a custom URL path |
| `--once` | Accept only one client and exit on disconnect |
| `--download <DIR>` | Serve files from directory at `/download/` |
| `--upload <DIR>` | Enable file upload to directory at `/upload` |
| `-6, --ipv6` | Enable IPv6 support |
| `-t, --tls` | Enable TLS with auto-generated self-signed certs |
| `--tls-cert <PATH>` | Custom TLS certificate path |
| `--tls-key <PATH>` | Custom TLS key path |
| `--tls-ca-crt <PATH>` | CA certificate for client authentication (mTLS) |
| `-2, --http2` | Enable HTTP/2 (requires TLS) |
| `-3, --http3` | Enable HTTP/3 QUIC (requires TLS) |

### Examples

**Secure session with random URL and Basic Auth:**

```bash
./sharetty -w -t -r -c admin:secret bash
```
*Access via: `https://<ip>:3000/<random_string>`*

**File sharing server:**

```bash
./sharetty --download ./files --upload ./uploads -w bash
```

**IPv6 only:**

```bash
./sharetty -6 -a ::1 bash
```

## Architecture

- **Backend**: Rust (`axum`, `tokio`, `h3`).
    - **Unix**: `pty-process`
    - **Windows**: `winpty-rs`
- **Frontend**: API-driven, embedding `xterm.js` via `rust-embed`.
- **Protocol**: WebSocket for PTY I/O, standard HTTP for static assets and file transfers.

## Reverse Connection (Relay Mode)

Share a terminal from behind a NAT or firewall by connecting OUT to a public Relay server.

### 1. Start the Relay (Public Server)
Run `sharetty` on a public server to listen for incoming agents and viewers.

```bash
# Listen on port 3000
./sharetty --url /relay -p 3000
```

### 2. Connect the Target (Private Machine)
Connect from the private machine to the Relay.

```bash
# Share 'bash' session with ID 'mysession'
./sharetty --connect http://relay-server:3000/relay --id mysession -w bash
```

### 3. Access from Browser
Viewers connect to the Relay's URL:
`http://relay-server:3000/relay/s/mysession/`

### File Transfer in Relay Mode
File transfer works seamlessly through the Relay.

**Enable on Target:**
```bash
./sharetty --connect http://relay-server:3000/relay \
  --id mysession \
  --download ./files \
  --upload ./uploads \
  -w bash
```

- **Download**: Access files at `.../s/mysession/download/filename`
- **Upload**: POST files to `.../s/mysession/upload`

## License

MIT

## References

-   [tty2web](https://github.com/kost/tty2web): share your terminal using tty2web
-   [gotty](https://github.com/yudai/gotty): Original gotty on which tty2web is based
-   [maintaned gotty](https://github.com/sorenisanerd/gotty): Maintained gotty
-   [Secure Shell (Chrome App)](https://chrome.google.com/webstore/detail/secure-shell/pnhechapfaindjhompbnflcldabbghjo): If you are a chrome user and need a "real" SSH client on your web browser, perhaps the Secure Shell app is what you want
-   [Wetty](https://github.com/krishnasrinivas/wetty): Node based web terminal (SSH/login)
-   [ttyd](https://tsl0922.github.io/ttyd): C port of GoTTY with CJK and IME support
