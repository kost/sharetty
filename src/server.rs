use axum::{
    response::{Html, IntoResponse, Response, Redirect},
    routing::{get, post, any},
    Router,
    http::{StatusCode, header, Uri, Request},
    body::Body,
    extract::{Multipart, Path, State, Query}, // Added Query
    extract::ws::{Message, WebSocket, WebSocketUpgrade}, // Added WS types
};
use tower::ServiceExt;
use std::net::SocketAddr;
use tower_http::{trace::TraceLayer, services::ServeFile};
use rust_embed::RustEmbed;
use axum_server::tls_rustls::RustlsConfig;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Notify, mpsc, broadcast, Mutex}; // Added sync primitives
use std::fs::File;
use std::io::BufReader;
use rustls::pki_types::PrivateKeyDer;
use rustls_pemfile::{certs, private_key};
use quinn::crypto::rustls::QuicServerConfig;
use axum::middleware::{self, Next};
use rand::{Rng, RngExt};
use base64::prelude::*;
use tokio::io::AsyncWriteExt;
use dashmap::DashMap; // Added DashMap
use futures::{sink::SinkExt, stream::StreamExt}; // Added futures traits
use serde::{Deserialize, Serialize};
use bytes::{BytesMut, Buf, BufMut}; // Added bytes for buffering

use crate::cli::Config;
use crate::websocket;
use crate::protocol::{ALPN_SHARETTY, HandshakeRequest, HandshakeResponse};

// Relay Messages
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage { // From Target Client
    Register { id: Option<String>, mode: String },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum ServerMessage { // To Target Client
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

pub struct Session {
    control_tx: mpsc::Sender<Message>,
    mode: String,
    // Broadcast Mode
    broadcast_tx: broadcast::Sender<Message>, // PTY output -> Viewers
    data_input_tx: Option<mpsc::Sender<Message>>, // Viewers -> Target Data (Set when Data connects)
    // Independent Mode
    // Map viewer_id -> (ws_sender to viewer) pending connection from target
    pending_viewers: DashMap<String, mpsc::Sender<Message>>,
}

use futures::stream::{SplitSink, SplitStream};

pub enum TunnelSender {
    WebSocket(SplitSink<WebSocket, Message>),
    Quic(quinn::SendStream),
}

impl TunnelSender {
    pub async fn send(&mut self, msg: Message) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self {
            TunnelSender::WebSocket(ws) => {
                ws.send(msg).await.map_err(|e| e.into())
            }
            TunnelSender::Quic(send) => {
                match msg {
                    Message::Binary(data) => {
                         let mut buf = Vec::with_capacity(5 + data.len());
                         buf.push(2u8); // Binary
                         let len = data.len() as u32;
                         buf.extend_from_slice(&len.to_be_bytes());
                         buf.extend_from_slice(&data);
                         send.write_all(&buf).await?;
                         Ok(())
                    }
                    Message::Text(text) => {
                         let data = text.as_bytes();
                         let mut buf = Vec::with_capacity(5 + data.len());
                         buf.push(1u8); // Text
                         let len = data.len() as u32;
                         buf.extend_from_slice(&len.to_be_bytes());
                         buf.extend_from_slice(data);
                         send.write_all(&buf).await?;
                         Ok(())
                    }
                     _ => Ok(()),
                }
            }
        }
    }
}

pub enum TunnelReceiver {
    WebSocket(SplitStream<WebSocket>),
    Quic(quinn::RecvStream, BytesMut),
}

impl TunnelReceiver {
    pub async fn recv(&mut self) -> Option<Result<Message, Box<dyn std::error::Error + Send + Sync>>> {
         match self {
            TunnelReceiver::WebSocket(ws) => {
                ws.next().await.map(|res| res.map_err(|e| e.into()))
            }
            TunnelReceiver::Quic(recv, buf) => {
                 loop {
                     // 1. Check if we have enough data for header
                     if buf.len() >= 5 {
                         let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
                         if buf.len() >= 5 + len {
                             // We have a full message
                             let msg_type = buf[0];
                             buf.advance(5); // Skip header
                             let val_buf = buf.copy_to_bytes(len).to_vec(); // Consume payload
                             
                             let msg = match msg_type {
                                 1 => {
                                     match String::from_utf8(val_buf) {
                                         Ok(s) => Message::Text(s),
                                         Err(e) => return Some(Err(e.into())),
                                     }
                                 }
                                 2 => {
                                     Message::Binary(val_buf)
                                 }
                                 _ => { 
                                     tracing::warn!("TR: Unknown message type: {}", msg_type);
                                     continue; 
                                 }
                             };
                             return Some(Ok(msg));
                         }
                     }
                     
                     // 2. Read more data
                     let mut chunk = [0u8; 4096];
                     match recv.read(&mut chunk).await {
                         Ok(Some(n)) => {
                             tracing::debug!("TR: Read {} bytes from stream.", n);
                             buf.put_slice(&chunk[..n]);
                             continue; // Loop back to check buffer
                         }
                         Ok(None) => {
                             if buf.is_empty() {
                                 return None; // EOF and no data left
                             } else {
                                 // EOF but data left? Incomplete message.
                                 tracing::warn!("TR: EOF with incomplete message: {} bytes", buf.len());
                                 return None;
                             }
                         }
                         Err(e) => {
                             tracing::error!("TR: Read failed: {}", e);
                             return Some(Err(e.into()));
                         }
                     }
                 }
            }
        }
    }
}

#[derive(Debug)]
pub enum Tunnel {
    WebSocket(WebSocket),
    Quic(quinn::SendStream, quinn::RecvStream),
}

impl Tunnel {
    pub fn split(self) -> (TunnelSender, TunnelReceiver) {
        match self {
            Tunnel::WebSocket(ws) => {
                let (tx, rx) = ws.split();
                (TunnelSender::WebSocket(tx), TunnelReceiver::WebSocket(rx))
            }
            Tunnel::Quic(send, recv) => {
                (TunnelSender::Quic(send), TunnelReceiver::Quic(recv, BytesMut::with_capacity(8192)))
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub shutdown_notify: Arc<Notify>,
    pub active_connections: Arc<AtomicUsize>,
    pub sessions: Arc<DashMap<String, Arc<Session>>>,
    pub tunnel_waiters: Arc<DashMap<String, tokio::sync::oneshot::Sender<Tunnel>>>,
}

#[derive(RustEmbed)]
#[folder = "static/"]
struct Asset;

pub async fn start(mut config: Config) {
    let shutdown_notify = Arc::new(Notify::new());
    let active_connections = Arc::new(AtomicUsize::new(0));
    let sessions = Arc::new(DashMap::new());
    let tunnel_waiters = Arc::new(DashMap::new());

    // Determine URL prefix
    let prefix = if let Some(url) = &config.url {
        if url.starts_with('/') { url.clone() } else { format!("/{}", url) }
    } else if config.random_url {
        let mut rng = rand::rng(); 
        let random_string: String = (0..config.random_url_length)
            .map(|_| {
                const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
                let idx = rng.random_range(0..CHARS.len());
                CHARS[idx] as char
            })
            .collect();
        let p = format!("/{}", random_string);
        config.url = Some(p.clone());
        p
    } else {
        "/".to_string()
    };
    
    let app_state = AppState { 
        config: config.clone(),
        shutdown_notify: shutdown_notify.clone(),
        active_connections: active_connections.clone(),
        sessions: sessions.clone(),
        tunnel_waiters: tunnel_waiters.clone(),
    };

    tracing::info!("Application served at path: {}", prefix);

    let mut app = Router::new()
        .route("/_sharetty/control", get(control_handler))
        .route("/_sharetty/data", get(data_handler))
        .route("/_sharetty/tunnel", get(tunnel_handler))
        .route("/s/:id/", get(viewer_index_handler))
        .route("/s/:id/ws", get(viewer_ws_handler))
        .route("/s/:id/download/*path", get(proxy_download_handler))
        .route("/s/:id/upload", get(upload_form_handler).post(proxy_upload_handler));

    if config.no_command {
        app = app.route("/", get(|| async { "Relay Mode Active" }));
    } else {
        app = app.route("/", any(index_handler))
                 .route("/ws", get(websocket::handler));
    }

    let app = app.fallback(static_handler)
        .layer(TraceLayer::new_for_http());

    let app = if config.upload.is_some() {
        app.route("/upload", get(upload_form_handler).post(upload_handler))
    } else {
        app
    };

    let app = if config.download.is_some() {
        app.route("/download", get(download_handler))
           .route("/download/", get(download_handler))
           .route("/download/*path", get(download_handler))
    } else {
        app
    };

    let mut app = app.with_state(app_state.clone());

    if config.credential.is_some() {
        app = app.layer(middleware::from_fn_with_state(app_state.clone(), auth_middleware));
    }
    
    if prefix != "/" {
        let p = prefix.trim_end_matches('/').to_string();
        let p_slash = format!("{}/", p);
        let redirect_target = p_slash.clone();
        app = Router::new()
            .route(&p, get(move || async move { Redirect::permanent(&redirect_target) }))
            .nest_service(&p_slash, app);
    }

    let ip = if config.ipv6 && config.address == std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED) {
        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        config.address
    };
    let addr = SocketAddr::new(ip, config.port);
    
    // Logic to determine TLS config
    let tls_data = if let (Some(cert_path), Some(key_path)) = (&config.tls_cert, &config.tls_key) {
        let certs = certs(&mut BufReader::new(File::open(cert_path).expect("cert file not found")))
            .collect::<Result<Vec<_>, _>>()
            .expect("invalid certs");
        let key = private_key(&mut BufReader::new(File::open(key_path).expect("key file not found")))
            .expect("invalid key")
            .expect("no private key found");
        Some((certs, key))
    } else if config.tls || config.http2 || config.http3 {
        tracing::info!("Generating self-signed certificate...");
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
        let certified_key = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();
        let cert_der = certified_key.cert.der().clone();
        let key_der = certified_key.signing_key.serialize_der();
        let certs = vec![cert_der];
        // rcgen produces PKCS8
        let key = PrivateKeyDer::Pkcs8(key_der.into());
        Some((certs, key))
    } else {
        None
    };

    if let Some((server_certs, server_key)) = tls_data {
        let mut protos = Vec::new();
        if config.http1 { protos.push("HTTP/1.1"); }
        if config.http2 { protos.push("HTTP/2"); }
        if config.http3 { protos.push("HTTP/3 QUIC"); }
        tracing::info!("Listening on https://{} ({})", addr, protos.join(", "));
        
        let builder = rustls::ServerConfig::builder();
        let builder = if let Some(ca_path) = &config.tls_ca_crt {
            let mut root_store = rustls::RootCertStore::empty();
            let certs = certs(&mut BufReader::new(File::open(ca_path).expect("ca cert file not found")))
                .collect::<Result<Vec<_>, _>>()
                .expect("invalid ca certs");
            for cert in certs {
                root_store.add(cert).expect("failed to add ca cert");
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder(root_store.into())
                .build()
                .expect("failed to build verifier");
            builder.with_client_cert_verifier(verifier)
        } else {
            builder.with_no_client_auth()
        };

        let mut server_config = builder
            .with_single_cert(server_certs, server_key)
            .unwrap();
            
        let mut alpn = Vec::new();
        if config.http3 { alpn.push(b"h3".to_vec()); }
        if config.http2 { alpn.push(b"h2".to_vec()); }
        if config.http1 { alpn.push(b"http/1.1".to_vec()); }
        alpn.push(ALPN_SHARETTY.to_vec());
        server_config.alpn_protocols = alpn;

        // RustlsConfig for axum-server (TCP)
        let tcp_ssl_config = Arc::new(server_config.clone());
        let tls_config = RustlsConfig::from_config(tcp_ssl_config);
 
        // TCP server future
        let app_tcp = app.clone();
        let addr_tcp = addr;
        
        let handle = axum_server::Handle::new();
        let handle_clone = handle.clone();
        let shutdown_signal = shutdown_notify.clone();
        tokio::spawn(async move {
            shutdown_signal.notified().await;
            handle_clone.graceful_shutdown(None);
        });

        let tcp_server = axum_server::bind_rustls(addr_tcp, tls_config)
            .handle(handle)
            .serve(app_tcp.into_make_service());

        if config.http3 {
            // Spawn TCP listener in background
            tokio::spawn(tcp_server);

            // Configure QUIC
            // Convert rustls config to quinn config
            let quic_crypto = QuicServerConfig::try_from(server_config).unwrap();
            let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
            // Enable address migration
            let mut transport_config = quinn::TransportConfig::default();
            transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(2)));
            transport_config.max_concurrent_bidi_streams(100u32.into());
            server_config.transport_config(Arc::new(transport_config));
            
            let endpoint = quinn::Endpoint::server(server_config, addr).unwrap();
            
            let shutdown_signal = shutdown_notify.clone();
            tokio::select! {
                _ = start_quic(endpoint, app, app_state) => {},
                _ = shutdown_signal.notified() => {},
            }
        } else {
            // Run TCP listener in foreground
            tcp_server.await.unwrap();
        }
    } else {
        tracing::info!("Listening on http://{} (HTTP/1.1)", addr);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let shutdown_signal = shutdown_notify.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_signal.notified().await;
            })
            .await.unwrap();
    }
}

async fn start_quic(endpoint: quinn::Endpoint, app: Router, app_state: AppState) {
    while let Some(conn) = endpoint.accept().await {
        let app = app.clone();
        let state = app_state.clone();
        tokio::spawn(async move {
            let connection = match conn.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("QUIC connection failed: {}", e);
                    return;
                }
            };
            
            println!("SERVER: QUIC Handshake complete. Extracting ALPN...");
            tracing::info!("QUIC Handshake complete. Extracting ALPN...");
            
            let handshake_data = match connection.handshake_data() {
                Some(d) => d,
                None => {
                     tracing::error!("No handshake data");
                     return;
                }
            };
            
            let alpn = match handshake_data.downcast::<quinn::crypto::rustls::HandshakeData>() {
                 Ok(d) => d.protocol.unwrap_or_default(),
                 Err(_) => {
                     tracing::error!("Failed to downcast handshake data");
                     return;
                 }
            };
            
            tracing::info!("Negotiated ALPN: {:?}", String::from_utf8_lossy(&alpn));

            if alpn == b"sharetty" {
                tracing::info!("Dispatching to handle_quic_connection");
                handle_quic_connection(connection, state).await;
                return;
            }
            
            let quinn_conn = h3_quinn::Connection::new(connection);
            let mut h3_conn = match h3::server::Connection::new(quinn_conn).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("H3 connection failed: {}", e);
                    return;
                }
            };

            loop {
                match h3_conn.accept().await {
                    Ok(Some(new_stream)) => {
                        let app = app.clone();
                        tokio::spawn(async move {
                            let (req, mut stream) = match new_stream.resolve_request().await {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::error!("Stream accept failed: {}", e);
                                    return;
                                }
                            };
                            
                            let (parts, _) = req.into_parts();
                            let axum_req = Request::from_parts(parts, Body::empty());
                            
                            match app.oneshot(axum_req).await {
                                Ok(response) => {
                                    let (parts, body) = response.into_parts();
                                    let parse_response = http::Response::from_parts(parts, ());
                                    
                                    match stream.send_response(parse_response).await {
                                        Ok(_) => {
                                            use http_body_util::BodyExt;
                                            if let Ok(collected) = body.collect().await {
                                               let bytes = collected.to_bytes();
                                               if let Err(e) = stream.send_data(bytes).await {
                                                    tracing::error!("Failed to send stream data: {}", e);
                                               }
                                            }
                                            if let Err(e) = stream.finish().await {
                                                 tracing::error!("Failed to finish stream: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                             tracing::error!("Failed to send response: {}", e);
                                        }
                                    }
                                }
                                Err(_) => {}
                            }
                        });
                    }
                    Ok(None) => break,
                    Err(_e) => {
                        // connection error
                        break;
                    }
                }
            }
        });
    }
}

async fn handle_quic_connection(connection: quinn::Connection, state: AppState) {
    let (send, recv) = match connection.accept_bi().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to accept control stream: {}", e);
            return;
        }
    };
    
    let mut rx = TunnelReceiver::Quic(recv, BytesMut::with_capacity(8192));
    let mut tx = TunnelSender::Quic(send);
    
    tracing::info!("HQ: Waiting for handshake...");
    // Handshake
    let handshake_req = match rx.recv().await {
        Some(Ok(Message::Text(json))) => {
             serde_json::from_str::<HandshakeRequest>(&json).ok()
        }
        _ => None,
    };
    
    let handshake_req = match handshake_req {
        Some(req) => req,
        None => {
            println!("SERVER: Handshake Invalid JSON");
            let _ = tx.send(Message::Text(serde_json::to_string(&HandshakeResponse { status: 400, error: Some("Invalid Handshake".into()) }).unwrap())).await;
            return;
        }
    };
    
    println!("SERVER: Handshake Valid: {:?}", handshake_req);

    if let Some(url) = &state.config.url {
        if !handshake_req.path.starts_with(url) {
             println!("SERVER: Handshake Invalid Path: {}", handshake_req.path);
             let _ = tx.send(Message::Text(serde_json::to_string(&HandshakeResponse { status: 404, error: Some("Invalid Path".into()) }).unwrap())).await;
             return;
        }
    }
    
    if let Some(cred) = &state.config.credential {
        if handshake_req.auth.as_deref() != Some(cred) {
             println!("SERVER: Handshake Unauthorized");
             let _ = tx.send(Message::Text(serde_json::to_string(&HandshakeResponse { status: 401, error: Some("Unauthorized".into()) }).unwrap())).await;
             return;
        }
    }
    
    println!("SERVER: Sending Handshake Response 200");
    if let Err(e) = tx.send(Message::Text(serde_json::to_string(&HandshakeResponse { status: 200, error: None }).unwrap())).await {
        println!("SERVER: Failed to send Handshake Response: {}", e);
        return;
    }
    println!("SERVER: Handshake Response Sent");

    loop {
        match rx.recv().await {
            Some(Ok(Message::Text(text))) => {
                 if let Ok(ClientMessage::Register { id, mode }) = serde_json::from_str::<ClientMessage>(&text) {
                     let sid = id.unwrap_or_else(|| {
                         let mut rng = rand::rng(); 
                         (0..8).map(|_| {
                             const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
                             let idx = rng.random_range(0..CHARS.len());
                             CHARS[idx] as char
                         }).collect()
                     });

                     tracing::info!("Registering QUIC session {} (mode: {})", sid, mode);
                     println!("SERVER: Registering session {}", sid);

                     let (control_tx, mut control_rx) = mpsc::channel(100);
                     let (broadcast_tx, _) = broadcast::channel(100);
                     
                     let session = Arc::new(Session {
                         control_tx: control_tx.clone(),
                         mode: mode.clone(),
                         broadcast_tx: broadcast_tx,
                         data_input_tx: None,
                         pending_viewers: DashMap::new(),
                     });
                     
                     state.sessions.insert(sid.clone(), session.clone());

                     let reply = ServerMessage::Registered { id: sid.clone() };
                     let _ = tx.send(Message::Text(serde_json::to_string(&reply).unwrap())).await;

                     let send_task = tokio::spawn(async move {
                        while let Some(msg) = control_rx.recv().await {
                            if tx.send(msg).await.is_err() { break; }
                        }
                     });
                     
                     let conn_handle = connection.clone();
                     let state_handle = state.clone();
                     tokio::spawn(async move {
                         while let Ok(bi) = conn_handle.accept_bi().await {
                             let (send, recv) = bi;
                             let tunnel_connection = Tunnel::Quic(send, recv);
                             tokio::spawn(handle_quic_tunnel_stream(tunnel_connection, state_handle.clone()));
                         }
                     });

                     while let Some(Ok(_)) = rx.recv().await {}
                     
                     send_task.abort();
                     state.sessions.remove(&sid);
                     tracing::info!("QUIC Session {} closed", sid);
                     break;
                 }
            }
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            _ => break,
        }
    }
}

async fn handle_quic_tunnel_stream(tunnel: Tunnel, state: AppState) {
    if let Tunnel::Quic(send, mut recv) = tunnel {
        // Read req_id (Type 1, Len, Data)
        let req_id = async {
            let mut type_buf = [0u8; 1];
            if recv.read_exact(&mut type_buf).await.is_err() { return None; }
            if type_buf[0] != 1 { return None; } // Expected Text
            
            let mut len_buf = [0u8; 4];
            if recv.read_exact(&mut len_buf).await.is_err() { return None; }
            let len = u32::from_be_bytes(len_buf) as usize;
            
            let mut val_buf = vec![0u8; len];
            if recv.read_exact(&mut val_buf).await.is_err() { return None; }
            
            String::from_utf8(val_buf).ok()
        }.await;
        
        if let Some(id) = req_id {
            if let Some((_, sender)) = state.tunnel_waiters.remove(&id) {
                // Reconstruct Tunnel
                let tunnel = Tunnel::Quic(send, recv);
                let _ = sender.send(tunnel);
            }
        }
    }
}

async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
    let index = Asset::get("index.html").map(|file| file.data);
    match index {
        Some(content) => {
            let mut html = String::from_utf8_lossy(&content).to_string();
            let upload_enabled = state.config.upload.is_some();
            let download_enabled = state.config.download.is_some();
            
            let config_script = format!(
                "<script>window.SHARETTY_CONFIG = {{ upload: {}, download: {} }};</script>",
                upload_enabled, download_enabled
            );
            
            if let Some(pos) = html.find("</head>") {
                html.insert_str(pos, &config_script);
            } else {
                 html.push_str(&config_script);
            }
            
            Html(html)
        },
        None => Html("Index not found".to_string()),
    }
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    
    match Asset::get(path) {
        Some(content) => {
            let body = Body::from(content.data);
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(body)
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap(),
    }
}

async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Some(cred) = &state.config.credential {
        let expected = format!("Basic {}", BASE64_STANDARD.encode(cred));
        
        let auth_header = req.headers().get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());
            
        if let Some(header_val) = auth_header {
            if header_val == expected {
                return next.run(req).await;
            }
        }
        
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"sharetty\"")
            .body(Body::from("Unauthorized"))
            .unwrap()
            .into_response();
    }
    next.run(req).await
}

async fn upload_form_handler() -> Html<&'static str> {
    crate::handlers::upload_form_handler().await
}

async fn upload_handler(
    State(state): State<AppState>,
    multipart: Multipart,
) -> impl IntoResponse {
    let upload_dir = PathBuf::from(state.config.upload.as_ref().unwrap());
    crate::handlers::upload_handler(upload_dir, multipart).await
}

async fn download_handler(
    State(state): State<AppState>,
    uri: axum::extract::OriginalUri,
    path: Option<Path<String>>,
    req: Request<Body>,
) -> Response {
    let download_dir = PathBuf::from(state.config.download.as_ref().unwrap());
    crate::handlers::download_handler(download_dir, uri, path, req).await
}

async fn control_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_control(socket, state))
}

async fn handle_control(mut socket: WebSocket, state: AppState) {
    if let Some(Ok(Message::Text(text))) = socket.recv().await {
        if let Ok(ClientMessage::Register { id, mode }) = serde_json::from_str::<ClientMessage>(&text) {
             let sid = id.unwrap_or_else(|| {
                 let mut rng = rand::rng(); 
                 (0..8).map(|_| {
                     const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
                     let idx = rng.random_range(0..CHARS.len());
                     CHARS[idx] as char
                 }).collect()
             });

             tracing::info!("Registering session {} (mode: {})", sid, mode);

             let (control_tx, mut control_rx) = mpsc::channel(100);
             let (broadcast_tx, _) = broadcast::channel(100);
             
             let session = Arc::new(Session {
                 control_tx: control_tx.clone(),
                 mode: mode.clone(),
                 broadcast_tx: broadcast_tx,
                 data_input_tx: None,
                 pending_viewers: DashMap::new(),
             });
             
             state.sessions.insert(sid.clone(), session.clone());

             let reply = ServerMessage::Registered { id: sid.clone() };
             let _ = socket.send(Message::Text(serde_json::to_string(&reply).unwrap())).await;

             let (mut sender, mut receiver) = socket.split();

             let send_task = tokio::spawn(async move {
                while let Some(msg) = control_rx.recv().await {
                    if sender.send(msg).await.is_err() { break; }
                }
             });

             while let Some(Ok(_)) = receiver.next().await {}
             
             send_task.abort();
             state.sessions.remove(&sid);
             tracing::info!("Session {} closed", sid);
         }
    }
}

async fn data_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_data(socket, state, params))
}

async fn handle_data(socket: WebSocket, state: AppState, params: std::collections::HashMap<String, String>) {
    let session_id = match params.get("session_id") {
        Some(id) => id,
        None => return,
    };
    
    let session = match state.sessions.get(session_id) {
        Some(s) => s.clone(),
        None => return,
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();

    if session.mode == "broadcast" {
        // Broadcast Mode
        // We need to capture the sender for Input (Viewer -> Target)
        // Since Session is shared, we can't easily set `data_input_tx`.
        // Workaround: Use pending_viewers with a special key to store the Input Sender.
        // The sender needs to be for `Message`.
        let (input_tx, mut input_rx) = mpsc::channel(100);
        session.pending_viewers.insert("BROADCAST_INPUT".to_string(), input_tx);

        // Task: Read from Input Channel -> Write to WS
        let input_task = tokio::spawn(async move {
            while let Some(msg) = input_rx.recv().await {
                if ws_sender.send(msg).await.is_err() { break; }
            }
        });

        // Task: Read from WS -> Broadcast
        while let Some(Ok(msg)) = ws_receiver.next().await {
             if let Message::Binary(_) = msg {
                  let _ = session.broadcast_tx.send(msg);
             }
        }
        
        input_task.abort();
        session.pending_viewers.remove("BROADCAST_INPUT");
    } else {
        // Independent Mode
        let viewer_id = params.get("viewer_id").cloned().unwrap_or_default();
        if let Some((_, viewer_tx)) = session.pending_viewers.remove(&viewer_id) {
            // Unidirectional: Target -> Viewer (via viewer_tx)
            // Input from viewer is missing here because we only stored Viewer's Sender.
            // For full bidirectional independent mode, we would need to store a bridge channel.
            // MVP: Output only or rely on separate channel?
            // Let's implement Output-only to verify connectivity first.
            
            while let Some(Ok(msg)) = ws_receiver.next().await {
                 let _ = viewer_tx.send(msg).await;
            }
        }
    }
}

async fn viewer_index_handler(Path(_id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    index_handler(State(state)).await 
}

async fn viewer_ws_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_viewer(socket, id, state))
}

async fn handle_viewer(socket: WebSocket, id: String, state: AppState) {
    let session = match state.sessions.get(&id) {
        Some(s) => s.clone(),
        None => return,
    };
    
    let (mut ws_sender, mut ws_receiver) = socket.split();

    if session.mode == "broadcast" {
        let mut rx = session.broadcast_tx.subscribe();
        
        // Output task
        let send_task = tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if ws_sender.send(msg).await.is_err() { break; }
            }
        });
        
        // Input task
        if let Some(input_tx) = session.pending_viewers.get("BROADCAST_INPUT") {
             let tx = input_tx.clone();
             while let Some(Ok(msg)) = ws_receiver.next().await {
                 let _ = tx.send(msg).await;
             }
        } else {
             // Just consume input if no target connected
             while let Some(Ok(_)) = ws_receiver.next().await {}
        }
        send_task.abort();
    } else {
        // Independent Mode
        let vid: String = rand::rng().sample_iter(&rand::distr::Alphanumeric).take(8).map(char::from).collect();
        
        let (to_viewer_tx, mut to_viewer_rx) = mpsc::channel(100);
        session.pending_viewers.insert(vid.clone(), to_viewer_tx);
        
        let cmd = ServerMessage::Spawn { viewer_id: vid.clone() };
        let _ = session.control_tx.send(Message::Text(serde_json::to_string(&cmd).unwrap())).await;
        
        let forward_task = tokio::spawn(async move {
            while let Some(msg) = to_viewer_rx.recv().await {
                if ws_sender.send(msg).await.is_err() { break; }
            }
        });
        
        // Input ignored for independent MVP (target -> viewer only)
        while let Some(Ok(_)) = ws_receiver.next().await {}
        
        forward_task.abort();
        session.pending_viewers.remove(&vid);
    }
}

async fn tunnel_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tunnel_socket(socket, state, params))
}

async fn handle_tunnel_socket(socket: WebSocket, state: AppState, params: std::collections::HashMap<String, String>) {
    let req_id = match params.get("req_id") {
        Some(id) => id,
        None => return,
    };
    
    if let Some((_, sender)) = state.tunnel_waiters.remove(req_id) {
        let _ = sender.send(Tunnel::WebSocket(socket));
    }
}

async fn proxy_request(state: AppState, session_id: String, method: String, uri: String, headers: header::HeaderMap, body: axum::body::Body) -> Response {

    let session = match state.sessions.get(&session_id) {
        Some(s) => s.clone(),
        None => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
    };

    let req_id: String = {
        let mut rng = rand::rng(); 
        (0..16).map(|_| {
            const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            let idx = rng.random_range(0..CHARS.len());
            CHARS[idx] as char
        }).collect()
    };
    
    let mut header_map = std::collections::HashMap::new();
    for (k, v) in headers.iter() {
        header_map.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
    }

    let cmd = ServerMessage::ProxyRequest {
        req_id: req_id.clone(),
        method,
        uri,
        headers: header_map,
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    state.tunnel_waiters.insert(req_id.clone(), tx);

    if let Err(_) = session.control_tx.send(Message::Text(serde_json::to_string(&cmd).unwrap())).await {
        state.tunnel_waiters.remove(&req_id);
        return (StatusCode::BAD_GATEWAY, "Failed to contact target").into_response();
    }

    let tunnel = match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(tunnel)) => tunnel,
        _ => {
            state.tunnel_waiters.remove(&req_id);
            return (StatusCode::GATEWAY_TIMEOUT, "Target timed out").into_response();
        }
    };

    let (mut tx, mut rx) = tunnel.split();

    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        while let Some(Ok(chunk)) = stream.next().await {
            if tx.send(Message::Binary(chunk.to_vec())).await.is_err() { break; }
        }
    });

    let mut meta_json = String::new();
    loop {
        match rx.recv().await {
            Some(Ok(Message::Text(text))) => {
                meta_json = text;
                break;
            }
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            Some(Err(e)) => {
                 tracing::error!("Tunnel error reading metadata: {}", e);
                 return (StatusCode::BAD_GATEWAY, "Tunnel error").into_response();
            }
            None => {
                 return (StatusCode::BAD_GATEWAY, "Stream closed before metadata").into_response();
            }
            _ => {
                 return (StatusCode::BAD_GATEWAY, "Unexpected message before metadata").into_response();
            }
        }
    }

    #[derive(Deserialize)]
    struct RespMeta { status: u16, headers: std::collections::HashMap<String, String> }
    
    if let Ok(meta) = serde_json::from_str::<RespMeta>(&meta_json) {
        let status = StatusCode::from_u16(meta.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut builder = Response::builder().status(status);
        for (k, v) in meta.headers {
            if let Ok(h_name) = header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(h_val) = header::HeaderValue::from_str(&v) {
                    builder = builder.header(h_name, h_val);
                }
            }
        }
        
        use futures::{Stream, StreamExt};
        let msg_stream = futures::stream::unfold(rx, |mut rx| async move {
            let res = rx.recv().await;
            res.map(|val| (val, rx))
        });
        
        let stream = msg_stream.filter_map(|msg| async {
            match msg {
                Ok(Message::Binary(data)) => Some(Ok::<_, Box<dyn std::error::Error + Send + Sync>>(axum::body::Bytes::from(data))),
                _ => None,
            }
        });
        
         let body = Body::from_stream(stream);
         return builder.body(body).unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, "Body error").into_response());
    }

    (StatusCode::BAD_GATEWAY, "Invalid response from target").into_response()
}


async fn proxy_download_handler(
    Path((id, path)): Path<(String, String)>,
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let uri = format!("/download/{}", path);
    proxy_request(state, id, parts.method.to_string(), uri, parts.headers, body).await
}


async fn proxy_upload_handler(
    Path(id): Path<String>,
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let uri = "/upload".to_string();
    proxy_request(state, id, parts.method.to_string(), uri, parts.headers, body).await
}
