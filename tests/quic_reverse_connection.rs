use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use std::fs;
use std::path::PathBuf;

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn test_quic_reverse_connection() {
    println!("TEST STARTING - UNIQUE ID: VERIFICATION_3019");
    let _ = tracing_subscriber::fmt().try_init();
    
    // Build the binary first
    let status = Command::new("cargo")
        .args(&["build", "--bin", "sharetty"])
        .status()
        .expect("Failed to build sharetty");
    assert!(status.success());
    
    let base_dir = std::env::temp_dir().join("sharetty_test_quic");
    if base_dir.exists() {
        std::fs::remove_dir_all(&base_dir).unwrap();
    }
    std::fs::create_dir_all(&base_dir).unwrap();
    let download_dir = base_dir.join("dl");
    let upload_dir = base_dir.join("up");
    std::fs::create_dir_all(&download_dir).unwrap();
    std::fs::create_dir_all(&upload_dir).unwrap();

    let file_content = "Hello from Target via QUIC";
    fs::write(download_dir.join("file.txt"), file_content).unwrap();
    
    let upload_content = "Hello from Viewer via Relay";
    let upload_file = base_dir.join("upload.txt"); // Changed from viewer_up to base_dir
    fs::write(&upload_file, upload_content).unwrap();

    // Start Relay with --http3
    println!("Starting Relay...");
    let relay_stdout = std::fs::File::create(base_dir.join("relay_stdout.txt")).unwrap();
    let relay_stderr = std::fs::File::create(base_dir.join("relay_stderr.txt")).unwrap();

    let relay_child = Command::new("target/debug/sharetty")
        .args(&["--url", "/sharetty", "-p", "3019", "--http3"])
        .stdout(relay_stdout)
        .stderr(relay_stderr)
        .spawn()
        .expect("Failed to start relay");
    
    let _relay_guard = ChildGuard(relay_child);

    // Wait for relay to start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Start Target (Client) with --use-quic
    println!("Starting Target...");
    let target_child = Command::new("target/debug/sharetty")
        .args(&[
            "--connect", "https://127.0.0.1:3019/sharetty",
            "--use-quic",
            "--download", download_dir.to_str().unwrap(),
            "--upload", upload_dir.to_str().unwrap(),
            "--id", "testquic"
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to start target");
    
    let _target_guard = ChildGuard(target_child);
    
    // Wait for target to register
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Test Download via HTTPS (curl)
    println!("Testing Download...");
    let output = Command::new("curl")
        .args(&["-v", "-k", "--tlsv1.2", "--http1.1", "https://127.0.0.1:3019/sharetty/s/testquic/download/file.txt"])
        .output()
        .expect("Failed to run curl download");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("Download Output: {}", stdout);
    println!("Download Stderr: {}", stderr);
    
    if stdout != file_content {
        println!("Expected content: {}", file_content);
        println!("Actual content: {}", stdout);
    }
    assert_eq!(stdout, file_content);

    // Test Upload
    println!("Testing Upload...");
    let output = Command::new("curl")
        .args(&[
            "-s", "-k", "--tlsv1.2", "--http1.1",
            "-F", &format!("file=@{}", upload_file.to_str().unwrap()),
            "https://127.0.0.1:3019/sharetty/s/testquic/upload"
        ])
        .output()
        .expect("Failed to run curl upload");
        
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Upload Output: {}", stdout);
    assert!(output.status.success());
    sleep(Duration::from_secs(2)).await;
    
    let uploaded_path = upload_dir.join("upload.txt");
    assert!(uploaded_path.exists());
    let content = fs::read_to_string(uploaded_path).unwrap();
    assert_eq!(content, upload_content);


    let _ = fs::remove_dir_all(&base_dir);
}
