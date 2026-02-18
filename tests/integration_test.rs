use tokio_tungstenite::connect_async;
use futures::StreamExt;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_server_connection() {
    // Find a free port
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    println!("Using port: {}", port);

    // Start the server in a separate process
    let mut child = std::process::Command::new("cargo")
        .args(&["run", "--", "-p", &port.to_string(), "-w", "echo", "hello"])
        .spawn()
        .expect("Failed to start server");

    // Give it some time to start, and retry connection
    let url = format!("ws://127.0.0.1:{}/ws", port);
    let mut retries = 10;
    let mut ws_stream = None;
    
    while retries > 0 {
        sleep(Duration::from_secs(1)).await;
        match connect_async(&url).await {
            Ok((stream, _)) => {
                ws_stream = Some(stream);
                break;
            }
            Err(_) => {
                retries -= 1;
            }
        }
    }
    let ws_stream = ws_stream.expect("Failed to connect to server after retries");
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
                 if output.contains("hello") {
                     break;
                 }
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
