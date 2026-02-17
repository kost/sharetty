use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use std::fs;
use std::path::PathBuf;

#[tokio::test]
async fn test_reverse_file_transfer() {
    let base_dir = std::env::temp_dir().join("sharetty_test");
    let target_dl = base_dir.join("dl");
    let target_up = base_dir.join("up");
    let viewer_up = base_dir.join("viewer");

    let _ = fs::remove_dir_all(&base_dir);
    fs::create_dir_all(&target_dl).unwrap();
    fs::create_dir_all(&target_up).unwrap();
    fs::create_dir_all(&viewer_up).unwrap();

    let file_content = "Hello from Target";
    fs::write(target_dl.join("file.txt"), file_content).unwrap();
    
    let upload_content = "Hello from Viewer";
    let upload_file = viewer_up.join("upload.txt");
    fs::write(&upload_file, upload_content).unwrap();

    // Start Relay
    let mut relay_child = Command::new("cargo")
        .args(&["run", "--", "--url", "/relay", "-p", "3009"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start relay");

    sleep(Duration::from_secs(10)).await;

    // Start Target
    let mut target_child = Command::new("cargo")
        .args(&[
            "run", "--", 
            "--connect", "http://localhost:3009/relay", 
            "--download", target_dl.to_str().unwrap(),
            "--upload", target_up.to_str().unwrap(),
            "--id", "testtarget"
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start target");

    sleep(Duration::from_secs(10)).await;

    // Test Download
    let output = Command::new("curl")
        .args(&["-v", "http://localhost:3009/relay/s/testtarget/download/file.txt"])
        .output()
        .expect("Failed to run curl download");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("Download Output: {}", stdout);
    println!("Download Stderr: {}", stderr);
    assert_eq!(stdout, file_content);

    // Test Upload
    let output = Command::new("curl")
        .args(&[
            "-s", 
            "-F", &format!("file=@{}", upload_file.to_str().unwrap()),
            "http://localhost:3009/relay/s/testtarget/upload"
        ])
        .output()
        .expect("Failed to run curl upload");
        
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Upload Output: {}", stdout);
    assert!(output.status.success());
    // Wait for file write? Axum handles it immediately?
    sleep(Duration::from_secs(1)).await;
    
    let uploaded_path = target_up.join("upload.txt");
    assert!(uploaded_path.exists());
    let content = fs::read_to_string(uploaded_path).unwrap();
    assert_eq!(content, upload_content);

    let _ = relay_child.kill();
    let _ = target_child.kill();
    let _ = fs::remove_dir_all(&base_dir);
}
