use crate::cli::Config;
use crate::websocket::ClientMessage;
use crate::protocol::{ALPN_SHARETTY, HandshakeRequest, HandshakeResponse};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tower::ServiceExt;
use std::path::PathBuf;
use std::sync::Arc;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum RegisterMessage {
    Register { id: Option<String>, mode: String },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum ServerMessage {
    Registered { id: String },
    Spawn { viewer_id: String },
    Error { message: String },
    ProxyRequest { 
        req_id: String,
        method: String,
        uri: String,
        headers: std::collections::HashMap<String, String>,
    },
}

pub async fn start(config: Config) {
    let connect_url = config.connect.as_ref().unwrap();
    let retry_interval = Duration::from_secs(config.retry);
    let mode = if config.independent { "independent" } else { "broadcast" };
    let requested_id = config.id.clone();
    
    // Parse URL
    let mut url = Url::parse(connect_url).expect("Invalid connect URL");
    if url.scheme().starts_with("http") {
        let new_scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(new_scheme).unwrap();
    }

    let mut router = Router::new();
    if let Some(dl) = &config.download {
        let dl_path = PathBuf::from(dl);
        router = router.route("/download/*path", get(client_download_handler).with_state(dl_path));
    }
    if let Some(ul) = &config.upload {
        let ul_path = PathBuf::from(ul);
        router = router.route("/upload", get(crate::handlers::upload_form_handler).post(client_upload_handler).with_state(ul_path));
    }
    
    if config.use_quic {
        connect_quic(config, router).await;
        return;
    }

    loop {
        tracing::info!("Connecting to relay at {}...", url);
        
        let mut control_url = url.clone();
        if !control_url.path().ends_with('/') {
            control_url.set_path(&format!("{}/", control_url.path()));
        }
        control_url = control_url.join("_sharetty/control").unwrap();

        match connect_async(control_url.to_string()).await {
            Ok((ws_stream, _)) => {
                tracing::info!("Connected to relay control channel.");
                let (mut write, mut read) = ws_stream.split();
                
                // Send Register
                let msg = RegisterMessage::Register {
                    id: requested_id.clone(),
                    mode: mode.to_string(),
                };
                let json = serde_json::to_string(&msg).unwrap();
                if let Err(e) = write.send(Message::Text(json)).await {
                     tracing::error!("Failed to send register: {}", e);
                     sleep(retry_interval).await;
                     continue;
                }

                let mut session_id = String::new();

                // Wait for Registration response
                while let Some(msg) = read.next().await {
                    if let Ok(Message::Text(text)) = msg {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(ServerMessage::Registered { id }) => {
                                tracing::info!("Registered session: {}", id);
                                session_id = id;
                                
                                // In Broadcast mode, start data connection immediately
                                if !config.independent {
                                    spawn_data_connection(url.clone(), session_id.clone(), None, config.clone());
                                }
                                break; // Go to main loop
                            }
                            Ok(ServerMessage::Error { message }) => {
                                tracing::error!("Registration failed: {}", message);
                                return; // Fatal error or retry?
                            }
                            _ => {}
                        }
                    }
                }
                
                if session_id.is_empty() {
                    tracing::error!("Failed to register session.");
                    sleep(retry_interval).await;
                    continue;
                }

                // Main Control Loop
                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(control) = serde_json::from_str::<ServerMessage>(&text) {
                                match control {
                                    ServerMessage::Spawn { viewer_id } => {
                                        if config.independent {
                                            tracing::info!("Spawning new PTY for viewer {}", viewer_id);
                                            spawn_data_connection(url.clone(), session_id.clone(), Some(viewer_id), config.clone());
                                        }
                                    }
                                    ServerMessage::ProxyRequest { req_id, method, uri, headers } => {
                                        let router = router.clone();
                                        let url = url.clone();
                                        tokio::spawn(async move {
                                            handle_proxy_request(url, req_id, method, uri, headers, router).await;
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Ok(Message::Close(_)) => {
                            tracing::warn!("Relay closed control connection.");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Control read error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to connect: {}", e);
            }
        }
        
        tracing::info!("Retrying in {} seconds...", config.retry);
        sleep(retry_interval).await;
    }
}

fn spawn_data_connection(start_url: Url, session_id: String, viewer_id: Option<String>, config: Config) {
    tokio::spawn(async move {
        let mut data_url = start_url.clone();
        if !data_url.path().ends_with('/') {
            data_url.set_path(&format!("{}/", data_url.path()));
        }
        data_url = data_url.join("_sharetty/data").unwrap();
        
        data_url.query_pairs_mut().append_pair("session_id", &session_id);
        if let Some(vid) = &viewer_id {
            data_url.query_pairs_mut().append_pair("viewer_id", vid);
        }

        match connect_async(data_url.to_string()).await {
            Ok((ws_stream, _)) => {
                let (mut ws_write, mut ws_read) = ws_stream.split();
                
                let cmd = if config.command.is_empty() { "bash".to_string() } else { config.command.clone() };
                let args = config.args.clone();

                // Using shared PTY abstraction
                match crate::pty::Pty::new(&cmd, &args) {
                    Ok((pty, mut child)) => {
                        let (mut pty_read, mut pty_write) = pty.into_split();
                        let mut buf = [0u8; 4096];
                        
                        // PTY -> WS
                        let ws_write_handle = tokio::spawn(async move {
                            loop {
                                match pty_read.read(&mut buf).await {
                                    Ok(n) if n > 0 => {
                                        if ws_write.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                                            break;
                                        }
                                    }
                                    _ => break,
                                }
                            }
                        });

                        // WS -> PTY
                        let ws_read_handle = tokio::spawn(async move {
                             while let Some(msg) = ws_read.next().await {
                                 match msg {
                                      Ok(Message::Binary(data)) => {
                                           if pty_write.write(&data).await.is_err() { break; }
                                      }
                                      Ok(Message::Text(text)) => {
                                          if let Ok(ClientMessage::Resize { rows, cols }) = serde_json::from_str::<ClientMessage>(&text) {
                                               let _ = pty_write.resize(rows, cols); 
                                          } else if let Ok(ClientMessage::Input { data }) = serde_json::from_str::<ClientMessage>(&text) {
                                               if config.permit_write {
                                                   let _ = pty_write.write(data.as_bytes()).await;
                                               }
                                          } else {
                                               // Fallback for raw text input if sent?
                                               if config.permit_write {
                                                   let _ = pty_write.write(text.as_bytes()).await;
                                               }
                                          }
                                      }
                                      _ => {}
                                 }
                             }
                        });
                        
                        let _ = tokio::join!(ws_write_handle, ws_read_handle);
                        let _ = child.kill().await;
                    }
                    Err(e) => tracing::error!("Failed to create PTY: {}", e),
                }
            }
            Err(e) => tracing::error!("Data connection failed: {}", e),
        }
    });
}

async fn handle_proxy_request(start_url: Url, req_id: String, method: String, uri: String, headers: std::collections::HashMap<String, String>, router: Router) {
    let mut tunnel_url = start_url.clone();
    if !tunnel_url.path().ends_with('/') {
        tunnel_url.set_path(&format!("{}/", tunnel_url.path()));
    }
    tunnel_url = tunnel_url.join("_sharetty/tunnel").unwrap();
    tunnel_url.query_pairs_mut().append_pair("req_id", &req_id);

    match connect_async(tunnel_url.to_string()).await {
        Ok((ws_stream, _)) => {
            let (mut ws_write, ws_read) = ws_stream.split();
            
            use futures::{StreamExt};
            let body_stream = ws_read.filter_map(|msg| async {
                match msg {
                    Ok(Message::Binary(data)) => Some(Ok::<_, std::io::Error>(axum::body::Bytes::from(data))),
                    _ => None,
                }
            });
            let body = Body::from_stream(body_stream);

            let mut builder = Request::builder()
                .method(axum::http::Method::from_bytes(method.as_bytes()).unwrap_or(axum::http::Method::GET))
                .uri(uri);
                
            for (k, v) in headers {
                if let Ok(bn) = axum::http::header::HeaderName::from_bytes(k.as_bytes()) {
                    if let Ok(bv) = axum::http::header::HeaderValue::from_str(&v) {
                        builder = builder.header(bn, bv);
                    }
                }
            }

            let req = builder.body(body).unwrap_or_default();

            let response = router.oneshot(req).await.unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Router Error").into_response());

            let (parts, body) = response.into_parts();
            
            let mut resp_headers = std::collections::HashMap::new();
            for (k, v) in parts.headers.iter() {
                resp_headers.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
            }

            #[derive(Serialize)]
            struct RespMeta { status: u16, headers: std::collections::HashMap<String, String> }
            
            let meta = RespMeta {
                status: parts.status.as_u16(),
                headers: resp_headers,
            };
            
            let _ = ws_write.send(Message::Text(serde_json::to_string(&meta).unwrap())).await;

            let mut body_stream = body.into_data_stream();
            while let Some(Ok(chunk)) = body_stream.next().await {
                if ws_write.send(Message::Binary(chunk.to_vec())).await.is_err() { break; }
            }
            let _ = ws_write.close().await;
        }
        Err(e) => tracing::error!("Failed to connect to tunnel: {}", e),
    }
}

async fn client_download_handler(
    State(dir): State<PathBuf>,
    uri: axum::extract::OriginalUri,
    match_path: Option<axum::extract::Path<String>>,
    req: axum::extract::Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts, body);
    crate::handlers::download_handler(dir, uri, match_path, req).await
}

async fn client_upload_handler(
    State(dir): State<PathBuf>,
    multipart: axum::extract::Multipart,
) -> impl axum::response::IntoResponse {
    crate::handlers::upload_handler(dir, multipart).await
}

#[derive(Debug)]
struct SkipServerVerification;
impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(&self, _end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _server_name: &ServerName<'_>, _ocsp_response: &[u8], _now: UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

async fn send_quic_msg(send: &mut quinn::SendStream, msg: Message) -> Result<(), std::io::Error> {
    match msg {
        Message::Text(t) => {
            let data = t.as_bytes();
            let mut buf = Vec::with_capacity(5 + data.len());
            buf.push(1u8);
            let len = data.len() as u32;
            buf.extend_from_slice(&len.to_be_bytes());
            buf.extend_from_slice(data);
            tracing::info!("Sending QUIC Text msg: {} bytes. Header: {:?}", buf.len(), &buf[0..std::cmp::min(10, buf.len())]);
            send.write_all(&buf).await.map_err(std::io::Error::other)?;
            send.flush().await.map_err(std::io::Error::other)
        }
        Message::Binary(b) => {
             let mut buf = Vec::with_capacity(5 + b.len());
            buf.push(2u8);
            let len = b.len() as u32;
            buf.extend_from_slice(&len.to_be_bytes());
            buf.extend_from_slice(&b);
            tracing::info!("Sending QUIC msg: {} bytes. Header: {:?}", buf.len(), &buf[0..std::cmp::min(10, buf.len())]);
            send.write_all(&buf).await.map_err(std::io::Error::other)?;
            send.flush().await.map_err(std::io::Error::other)
        }
        _ => Ok(())
    }
}

async fn recv_quic_msg(recv: &mut quinn::RecvStream) -> Option<Result<Message, std::io::Error>> {
    let mut type_buf = [0u8; 1];
    if let Err(e) = recv.read_exact(&mut type_buf).await {
         if let quinn::ReadExactError::FinishedEarly(_) = e {
             return None;
         }
        tracing::error!("Recv type failed: {}", e);
        return None; 
    }
    let msg_type = type_buf[0];
    
    let mut len_buf = [0u8; 4];
    if let Err(e) = recv.read_exact(&mut len_buf).await {
         tracing::error!("Recv len failed: {}", e);
         return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    
    let mut val_buf = vec![0u8; len];
    if recv.read_exact(&mut val_buf).await.is_err() { return None; }
    
    match msg_type {
        1 => {
             let s = String::from_utf8(val_buf).ok()?;
             Some(Ok(Message::Text(s)))
        }
        2 => Some(Ok(Message::Binary(val_buf))),
        _ => None
    }
}

async fn connect_quic(config: Config, router: Router) {
    let connect_url = config.connect.as_ref().unwrap();
    let url = Url::parse(connect_url).expect("Invalid connect URL");
    let host = url.host_str().expect("URL missing host");
    let port = url.port().unwrap_or(443);
    
    let mut client_crypto; 
    
    if let Some(ca_path) = &config.tls_ca_crt {
         let f = std::fs::File::open(ca_path).expect("ca cert not found");
         let mut reader = std::io::BufReader::new(f);
         let certs = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>().unwrap();
         let mut roots = rustls::RootCertStore::empty();
         for cert in certs { roots.add(cert).unwrap(); }
         client_crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
    } else {
        client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();
    }

    client_crypto.alpn_protocols = vec![ALPN_SHARETTY.to_vec()];
    
    let quic_client_config = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto).unwrap();
    let client_config = quinn::ClientConfig::new(Arc::new(quic_client_config));
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    
    let retry_interval = Duration::from_secs(config.retry);
    let mode = if config.independent { "independent" } else { "broadcast" };
    let requested_id = config.id.clone();
    
    loop {
        tracing::info!("Resolving {}...", host);
        let addrs = match tokio::net::lookup_host((host, port)).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("DNS lookup failed: {}", e);
                sleep(retry_interval).await;
                continue;
            }
        };
        let remote_addr = match addrs.into_iter().next() {
            Some(a) => a,
            None => {
                tracing::error!("No address resolved");
                sleep(retry_interval).await;
                continue;
            }
        };

        tracing::info!("Connecting via QUIC to {} ({})", connect_url, remote_addr);
        
        let connection = match endpoint.connect(remote_addr, host) {
            Ok(c) => match c.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Failed to connect: {}", e);
                    sleep(retry_interval).await;
                    continue;
                }
            },
            Err(e) => {
                 tracing::error!("Endpoint error: {}", e);
                 sleep(retry_interval).await;
                 continue;
            }
        };
        
        tracing::info!("QUIC Connected. Opening Control Stream...");

        let (mut send, mut recv) = match connection.open_bi().await {
            Ok(bi) => bi,
            Err(e) => {
                 tracing::error!("Failed to open control stream: {}", e);
                 continue;
            }
        };
        
        let handshake = HandshakeRequest {
            path: url.path().to_string(),
            auth: config.credential.clone(),
            version: "1.0".to_string(),
        };
        
        let json = serde_json::to_string(&handshake).unwrap();
        if let Err(e) = send_quic_msg(&mut send, Message::Text(json)).await {
            tracing::error!("Handshake send failed: {}", e);
            continue;
        }
        
        if let Some(Ok(Message::Text(resp_json))) = recv_quic_msg(&mut recv).await {
            if let Ok(resp) = serde_json::from_str::<HandshakeResponse>(&resp_json) {
                if resp.status != 200 {
                    tracing::error!("Handshake rejected: {} {:?}", resp.status, resp.error);
                    sleep(retry_interval).await;
                    continue;
                }
            } else {
                tracing::error!("Invalid handshake response");
                continue;
            }
        } else {
             tracing::error!("Handshake failed (no response)");
             sleep(retry_interval).await;
             continue;
        }
        
        tracing::info!("Handshake successful. Registering...");

        // Send Register
        let msg = RegisterMessage::Register {
            id: requested_id.clone(),
            mode: mode.to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        if let Err(e) = send_quic_msg(&mut send, Message::Text(json)).await {
             tracing::error!("Failed to send register: {}", e);
             continue;
        }
        
        let mut session_id = String::new();
        while let Some(msg) = recv_quic_msg(&mut recv).await {
            if let Ok(Message::Text(text)) = msg {
                if let Ok(ServerMessage::Registered { id }) = serde_json::from_str::<ServerMessage>(&text) {
                    tracing::info!("Registered session: {}", id);
                    session_id = id;
                    break;
                }
            }
        }
        
        if session_id.is_empty() {
             tracing::error!("Registration failed (no ID)");
             continue;
        }
        
        tracing::info!("Session established. Listening for commands (QUIC)...");
        
        let connection_handle = connection.clone();
        let router_handle = router.clone();
        
        // Read loop
        while let Some(msg) = recv_quic_msg(&mut recv).await {
             if let Ok(Message::Text(text)) = msg {
                if let Ok(cmd) = serde_json::from_str::<ServerMessage>(&text) {
                    match cmd {
                        ServerMessage::Spawn { viewer_id: _ } => {
                        },
                        ServerMessage::ProxyRequest { req_id, method, uri, headers } => {
                             let conn = connection_handle.clone();
                             let r = router_handle.clone();
                             tokio::spawn(async move {
                                 match conn.open_bi().await {
                                     Ok((mut tx, rx)) => {
                                         if (send_quic_msg(&mut tx, Message::Text(req_id)).await).is_err() { return; }
                                         
                                         handle_proxy_request_quic(r, method, uri, headers, tx, rx).await;
                                     }
                                     Err(e) => tracing::error!("Failed to open tunnel stream: {}", e),
                                 }
                             });
                        },
                        _ => {}
                    }
                } 
             }
        }
        tracing::info!("Control channel closed. Reconnecting...");
        sleep(retry_interval).await;
    }
}

async fn handle_proxy_request_quic(
    router: Router, 
    method: String, 
    uri: String, 
    headers: std::collections::HashMap<String, String>,
    mut tx: quinn::SendStream,
    rx: quinn::RecvStream
) {
    let req_builder = Request::builder()
        .method(axum::http::Method::from_bytes(method.as_bytes()).unwrap_or(axum::http::Method::GET))
        .uri(uri);
        
    let mut req_builder = req_builder;
    for (k, v) in headers {
        if let Ok(hn) = axum::http::header::HeaderName::from_bytes(k.as_bytes()) {
            if let Ok(hv) = axum::http::header::HeaderValue::from_str(&v) {
                req_builder = req_builder.header(hn, hv);
            }
        }
    }
    
    let body_stream = futures::stream::unfold(rx, |mut rx| async move {
        match recv_quic_msg(&mut rx).await {
            Some(Ok(Message::Binary(data))) => Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(data)), rx)),
            _ => None
        }
    });
    let body = Body::from_stream(body_stream);
    
    let req = req_builder.body(body).unwrap_or_default();
    
    let response = router.oneshot(req).await.unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Router Error").into_response());
    
    let mut header_map = std::collections::HashMap::new();
    for (k, v) in response.headers() {
        header_map.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
    }
    
    #[derive(Serialize)]
    struct RespMeta { status: u16, headers: std::collections::HashMap<String, String> }
    
    let meta = RespMeta {
        status: response.status().as_u16(),
        headers: header_map,
    };
    
    let meta_json = serde_json::to_string(&meta).unwrap();
    if (send_quic_msg(&mut tx, Message::Text(meta_json)).await).is_err() { return; }
    
    let mut stream = response.into_body().into_data_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        if (send_quic_msg(&mut tx, Message::Binary(chunk.to_vec())).await).is_err() { break; }
    }
    let _ = tx.finish();
}
