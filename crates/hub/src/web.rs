use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use std::env;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use crate::state::SharedState;

const INDEX_HTML: &str = include_str!("ui/index.html");

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX_HTML)
}

async fn api_status(State(state): State<SharedState>) -> impl IntoResponse {
    let st = state.read().await;
    Json(st.to_status())
}

// ---------------------------------------------------------------------------
// Server entry-point
// ---------------------------------------------------------------------------

pub async fn serve(state: SharedState) {
    let port: u16 = env::var("WEB_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await.expect("failed to bind web port");

    eprintln!("web ui listening on http://{addr}");

    axum::serve(listener, router(state))
        .await
        .expect("web server error");
}
