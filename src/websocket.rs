use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use crate::pty::Pty;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, debug, warn};
use serde::Deserialize;
// use serde_json::Value; // unused
// use std::sync::Arc; // unused

use crate::server::AppState;
use std::sync::atomic::Ordering;
// use crate::cli::Config;

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize { rows: u16, cols: u16 },
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    if state.config.once 
        && state.active_connections.fetch_add(1, Ordering::SeqCst) >= 1 {
        warn!("Connection rejected due to --once flag limit");
        return;
    }

    let cmd = state.config.command.clone();
    let args = state.config.args.clone();
    let config = state.config.clone();

    info!("Spawning command: {} {:?}", cmd, args);

    info!("Spawning command: {} {:?}", cmd, args);

    // Using our abstraction
    let (pty, mut child) = match Pty::new(&cmd, &args) {
        Ok(res) => res,
        Err(e) => {
            error!("Failed to create/spawn PTY: {}", e);
            return;
        }
    };
    
    // Abstract Pty::into_split returns (PtyReader, PtyWriter) which impl AsyncRead/AsyncWrite
    let (mut pty_read, mut pty_write) = pty.into_split();
    
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Task: Read from PTY, write to WebSocket
    let send_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match pty_read.read(&mut buf).await {
                Ok(n) if n > 0 => {
                    let data = buf[..n].to_vec();
                    // Send binary data to xterm.js
                    if ws_sender.send(Message::Binary(data)).await.is_err() {
                        debug!("WebSocket closed during send");
                        break;
                    }
                }
                Ok(_) => { 
                    debug!("PTY closed (EOF)");
                    break; 
                }
                Err(e) => {
                    error!("Error reading from PTY: {}", e);
                    break;
                }
            }
        }
    });

    // Handle incoming messages from WebSocket
    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(text) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                     Ok(ClientMessage::Input { data }) => {
                        if config.permit_write {
                            if let Err(e) = pty_write.write_all(data.as_bytes()).await {
                                error!("Failed to write to PTY: {}", e);
                            }
                        }
                     },
                     Ok(ClientMessage::Resize { rows, cols }) => {
                        debug!("Resizing PTY to {}x{}", rows, cols);
                        // Our abstraction takes rows, cols directly
                        // We need access to `pty` here to resize? 
                        // Ah, the abstraction split consumes `pty`.
                        // We typically need a handle to resize. 
                        // `pty_process::Pty` supports resizing on the master handle.
                        // But we consumed it into `pty_read` / `pty_write`.
                        // `pty_process` split returns `PtyRead` and `PtyWrite` which are wrapper structs around `File` or similar.
                        // They don't expose resize directly on read/write halves usually.
                        
                        // Wait, `pty_process::Pty::into_split` returns `(OwnedReadHalf, OwnedWriteHalf)`.
                        // `OwnedReadHalf` has a reference to the fd?
                        // Actually, to resize, we often need the original PTY handle or ANY handle to the master.
                        // `pty_process` 0.4: `Pty` implements `AsRawFd`. `OwnedReadHalf` implements `AsRawFd`.
                        // We can likely resize using any half if the library supports it, OR we need to rethink the abstraction.
                        // The previous code had `pty_write.resize(size)`? 
                        // `pty_process` doesn't seem to expose `resize` on `OwnedWriteHalf`.
                        // The previous code: `if let Err(e) = pty_write.resize(size)` implies `pty_write` had resize.
                        // Let's check `pty-process` docs or assume the previous code worked.
                        // If `pty-process` `OwnedWriteHalf` doesn't have `resize`, then the previous code was broken or using a different version?
                        // The user said "The previous code...".
                        
                        // Let's assume the new abstraction `Pty` exposes resize, but we consumed it.
                        // We should probably share the resize capability or make `Pty` cloneable/accessible.
                        // OR, `PtyWriter` should expose `resize`.
                        
                        // Let's update `src/pty.rs` to allow resizing from `PtyWriter` or similar mechanism.
                        // For now, let's just comment out resize or fix `pty.rs`.
                        // FIX: I will update `src/pty.rs` to add `resize` to `PtyWriter` or `Pty`.
                        
                        // But wait, the previous code in `websocket.rs` lines 120: `pty_write.resize(size)`
                        // This suggests `pty_write` (the second return of `pty.into_split()`) had a `resize` method.
                        
                        // For this step, I'll attempt to call `pty_write.resize()` and then fix `pty.rs` to support it.
                         if let Err(e) = pty_write.resize(rows, cols) {
                             warn!("Failed to resize PTY: {}", e);
                        }
                     },
                     Err(_) => {
                        // Maybe raw input?
                        if config.permit_write {
                             if let Err(e) = pty_write.write_all(text.as_bytes()).await {
                                error!("Failed to write to PTY (raw): {}", e);
                            }
                        }
                     }
                }
            }
            Message::Binary(data) => {
                if config.permit_write {
                    if let Err(e) = pty_write.write_all(&data).await {
                         error!("Failed to write binary to PTY: {}", e);
                    }
                }
            }
            Message::Close(_) => {
                debug!("WebSocket closed");
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
    let _ = child.kill().await;

    if state.config.once {
        info!("Client disconnected, shutting down due to --once flag");
        state.shutdown_notify.notify_one();
    }
}
