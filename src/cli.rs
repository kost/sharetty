use clap::Parser;
use std::net::IpAddr;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    /// Command to execute
    #[arg(default_value = "bash")]
    pub command: String,

    /// Arguments for the command
    pub args: Vec<String>,

    /// Listening address
    #[arg(short, long, default_value = "0.0.0.0")]
    pub address: IpAddr,

    /// Listening port
    #[arg(short, long, default_value_t = 3000)]
    pub port: u16,

    /// TLS certificate path (required for HTTP/2 and QUIC)
    #[arg(long)]
    pub tls_cert: Option<String>,

    /// TLS key path (required for HTTP/2 and QUIC)
    #[arg(long)]
    pub tls_key: Option<String>,

    /// Enable TLS (auto-generate certs if not provided)
    #[arg(short = 't', long)]
    pub tls: bool,

    /// TLS CA certificate for client verification
    #[arg(long)]
    pub tls_ca_crt: Option<String>,

    /// Allow clients to write to the TTY
    #[arg(short = 'w', long)]
    pub permit_write: bool,

    /// Credential for Basic Authentication (ex: user:pass, default disabled)
    #[arg(short = 'c', long)]
    pub credential: Option<String>,

    /// Add a random string to the URL
    #[arg(short = 'r', long)]
    pub random_url: bool,

    /// Random URL length
    #[arg(long, default_value_t = 8)]
    pub random_url_length: usize,

    /// Specify string for the URL
    #[arg(long)]
    pub url: Option<String>,

    /// Run as a public relay without a local command
    #[arg(long)]
    pub no_command: bool,

    /// Accept only one client and exit on disconnection
    #[arg(long)]
    pub once: bool,

    /// Serve files to download from specified dir
    #[arg(long)]
    pub download: Option<String>,

    /// Enable uploading of files to the specified dir
    #[arg(long)]
    pub upload: Option<String>,

    /// Connect to a relay server (reverse connection)
    #[arg(long)]
    pub connect: Option<String>,

    /// Connect using QUIC (direct) instead of WebSocket
    #[arg(long)]
    pub use_quic: bool,

    /// Session ID for reverse connection
    #[arg(long)]
    pub id: Option<String>,

    /// Reconnect retry interval in seconds
    #[arg(long, default_value_t = 5)]
    pub retry: u64,

    /// Enable independent session mode (new PTY per viewer)
    #[arg(long, conflicts_with = "broadcast")]
    pub independent: bool,

    /// Enable broadcast session mode (shared PTY, default)
    #[arg(long, conflicts_with = "independent")]
    pub broadcast: bool,

    /// Enable IPv6
    #[arg(short = '6', long)]
    pub ipv6: bool,

    /// Enable IPv4
    #[arg(short = '4', long)]
    pub ipv4: bool,

    /// Enable HTTP/1.1
    #[arg(short = '1', long, default_value_t = true)]
    pub http1: bool,

    /// Enable HTTP/2 (requires TLS)
    #[arg(short = '2', long)]
    pub http2: bool,

    /// Enable HTTP/3 (QUIC) (requires TLS)
    #[arg(long)]
    pub http3: bool,

    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Log to file
    #[arg(short = 'l', long = "log")]
    pub log_file: Option<String>,
}
