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
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
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
    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind web port");

    eprintln!("web ui listening on http://{addr}");

    axum::serve(listener, router(state))
        .await
        .expect("web server error");
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SystemState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tower::ServiceExt; // for `oneshot`

    /// Build a SharedState with two zones for testing.
    fn test_state() -> SharedState {
        let zones = vec![("zone1".to_string(), 17), ("zone2".to_string(), 27)];
        Arc::new(RwLock::new(SystemState::new(&zones)))
    }

    #[tokio::test]
    async fn index_returns_html() {
        let app = router(test_state());
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn api_status_returns_json_with_expected_fields() {
        let app = router(test_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["uptime_secs"].is_u64());
        assert!(json["mqtt_connected"].is_boolean());
        assert!(json["nodes"].is_object());
        assert!(json["zones"].is_object());
        assert!(json["events"].is_array());
        // Should have our two zones
        assert!(json["zones"]["zone1"].is_object());
        assert!(json["zones"]["zone2"].is_object());
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = router(test_state());
        let req = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
