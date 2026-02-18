use axum::{
    body::Body,
    extract::{Multipart, Path},
    http::{StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use std::path::{Path as StdPath, PathBuf};
use tower_http::services::ServeFile;
use tokio::fs;
use tower::ServiceExt;

// Re-use Asset from server? Or define simple asset reading interface?
// For now, let's assume we can move Asset or pass content.
// Actually `Asset` is `RustEmbed`. We can import it if we make it pub or move it.
// Better: Move `Asset` struct to a `common` module or `handlers` if it's used here.
// `server.rs` uses `Asset` for index/static.
// `handlers` uses it for directory listing? No, `download_handler` generates HTML.
// `download_handler` does NOT use `Asset`.
// `upload_form_handler` uses inline HTML.
// So `handlers.rs` doesn't need `Asset`.

pub async fn upload_form_handler() -> Html<&'static str> {
    Html(r#"
        <!doctype html>
        <title>Upload File</title>
        <h1>Upload File</h1>
        <form action="" method="post" enctype="multipart/form-data">
            <p><input type="file" name="file" multiple></p>
            <p><input type="submit" value="Upload"></p>
        </form>
    "#)
}

pub async fn upload_handler(
    upload_dir: PathBuf,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Ensure upload directory exists
    if let Err(e) = fs::create_dir_all(&upload_dir).await {
         return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create upload dir: {}", e)).into_response();
    }
    
    while let Ok(Some(field)) = multipart.next_field().await {
        let file_name = if let Some(name) = field.file_name() {
            name.to_string()
        } else {
            continue;
        };
        
        let file_name = StdPath::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let path = upload_dir.join(&file_name);
        
        let data = match field.bytes().await {
            Ok(data) => data,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("Failed to read field: {}", e)).into_response(),
        };

        if let Err(e) = fs::write(&path, &data).await {
             return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write file: {}", e)).into_response();
        }
    }
    
    "Upload successful".into_response()
}

pub async fn download_handler(
    download_dir: PathBuf,
    axum::extract::OriginalUri(original_uri): axum::extract::OriginalUri,
    path: Option<Path<String>>,
    req: axum::http::Request<Body>,
) -> Response {
    let req_path = path.map(|p| p.0).unwrap_or_default();
    
    // Prevent directory traversal
    if req_path.contains("..") {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let full_path = download_dir.join(&req_path);

    if full_path.is_dir() {
        // Enforce trailing slash
        let uri_path = original_uri.path();
        if !uri_path.ends_with('/') {
            return Redirect::permanent(&format!("{}/", uri_path)).into_response();
        }

        let mut html = String::from("<!doctype html><html><head><title>Index of /");
        html.push_str(&req_path);
        html.push_str("</title></head><body><h1>Index of /");
        html.push_str(&req_path);
        html.push_str("</h1><hr><ul>");
        
        if !req_path.is_empty() {
             html.push_str("<li><a href=\"..\">..</a></li>");
        }
        
        if let Ok(entries) = std::fs::read_dir(&full_path) {
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());

            for entry in entries {
                 let name = entry.file_name().to_string_lossy().to_string();
                 let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                 let display_name = if is_dir { format!("{}/", name) } else { name.clone() };
                 
                 html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", name, display_name));
            }
        }
        html.push_str("</ul><hr></body></html>");
        return Html(html).into_response();
    }
    
    match ServeFile::new(full_path).oneshot(req).await {
         Ok(res) => res.into_response(),
         Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
    }
}
