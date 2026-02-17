use tokio_tungstenite::connect_async;
use futures::StreamExt;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_server_connection() {
    // Start the server in a separate process
    let mut child = std::process::Command::new("cargo")
        .args(&["run", "--", "-p", "3001", "-w", "echo", "hello"])
        .spawn()
        .expect("Failed to start server");

    // Give it some time to start
    sleep(Duration::from_secs(2)).await;

    // Connect to WebSocket
    let url = "ws://127.0.0.1:3001/ws";
    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");
    let (_write, mut read) = ws_stream.split();

    // Send a message (if needed, but echo should just print and exit?)
    // Actually "echo hello" runs and exits.
    
    // We expect some output "hello\r\n" probably.
    
    let mut output = String::new();
    while let Some(msg) = read.next().await {
        if let Ok(msg) = msg {
            if msg.is_binary() {
                 let data = msg.into_data();
                 output.push_str(&String::from_utf8_lossy(&data));
            } else if msg.is_close() {
                break;
            }
        }
    }

    // Kill server
    child.kill().expect("Failed to kill server");

    println!("Output: {}", output);
    assert!(output.contains("hello"));
}
