use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/build/"]
struct Assets;

pub async fn static_handler(req: Request<Body>) -> impl IntoResponse {
    let mut path = req.uri().path();

    if !path.starts_with("/E") && !path.starts_with("/e") {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/E/")
            .body(Body::empty())
            .expect("static redirect response");
    }

    if path.starts_with("/E/") || path.starts_with("/e/") {
        path = &path[3..];
    } else if path == "/E" || path == "/e" {
        path = "";
    }

    let path = path.trim_start_matches('/');
    let lookup_path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(lookup_path) {
        Some(content) => {
            let mime = mime_guess::from_path(lookup_path).first_or_octet_stream();
            let cache_control = if lookup_path.starts_with("i/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };

            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache_control)
                .body(Body::from(content.data.to_vec()))
                .expect("static asset response")
        }
        None => match Assets::get("index.html") {
            Some(content) => Response::builder()
                .header(header::CONTENT_TYPE, "text/html")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(content.data.to_vec()))
                .expect("static fallback response"),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("404 Not Found (Frontend build missing)"))
                .expect("static 404 response"),
        },
    }
}
