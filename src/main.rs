use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cli;
mod server;
mod client;
mod websocket;
mod handlers;
mod protocol;
mod pty;

#[tokio::main]
async fn main() {
    // Install default crypto provider for rustls
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = cli::Config::parse();

    let log_level = match config.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| log_level.into()),
    );

    let registry = tracing_subscriber::registry().with(filter);

    if let Some(log_file) = &config.log_file {
        let file_appender = tracing_appender::rolling::never(".", log_file);
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        
        // We need to keep _guard alive, but main returns quickly? 
        // Actually for non-blocking we need to return the guard or keep it.
        // For simplicity and immediate flushing given "simple file OUT" request, 
        // let's use a blocking file writer or just let the guard drop at end of main?
        // Wait, main runs the server. So we can keep guard in main scope.
        
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false);

        let stdout_layer = tracing_subscriber::fmt::layer();

        registry
            .with(stdout_layer)
            .with(file_layer)
            .init();

        // Keep guard alive
        if config.connect.is_some() {
            client::start(config).await;
        } else {
            server::start(config).await;
        }
    } else {
        let stdout_layer = tracing_subscriber::fmt::layer();
        registry.with(stdout_layer).init();

        if config.connect.is_some() {
            client::start(config).await;
        } else {
            server::start(config).await;
        }
    }
}
