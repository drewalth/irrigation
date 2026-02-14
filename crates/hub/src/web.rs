use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use std::env;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use crate::db::Db;
use crate::state::SharedState;

const INDEX_HTML: &str = include_str!("ui/index.html");

// ---------------------------------------------------------------------------
// Composite app state shared across all handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppState {
    pub shared: SharedState,
    pub db: Db,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/zones", get(api_zones))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn api_status(State(state): State<AppState>) -> impl IntoResponse {
    let st = state.shared.read().await;
    Json(st.to_status())
}

async fn api_zones(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.load_zones().await {
        Ok(zones) => Json(serde_json::json!(zones)),
        Err(e) => {
            eprintln!("api_zones error: {e}");
            Json(serde_json::json!({ "error": e.to_string() }))
        }
    }
}

// ---------------------------------------------------------------------------
// Server entry-point
// ---------------------------------------------------------------------------

pub async fn serve(shared: SharedState, db: Db) {
    let port: u16 = env::var("WEB_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind web port");

    eprintln!("web ui listening on http://{addr}");

    let state = AppState { shared, db };
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
    use crate::db::Db;
    use crate::state::SystemState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tower::ServiceExt; // for `oneshot`

    /// Build an AppState backed by an in-memory SQLite DB for testing.
    async fn test_state() -> AppState {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();

        let zones = vec![("zone1".to_string(), 17), ("zone2".to_string(), 27)];
        let shared = Arc::new(RwLock::new(SystemState::new(&zones)));

        AppState { shared, db }
    }

    #[tokio::test]
    async fn index_returns_html() {
        let app = router(test_state().await);
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
        let app = router(test_state().await);
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
    async fn api_zones_returns_empty_array_when_no_zones() {
        let app = router(test_state().await);
        let req = Request::builder()
            .uri("/api/zones")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_array());
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn api_zones_returns_inserted_zone() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&crate::db::ZoneConfig {
                zone_id: "z1".into(),
                name: "Test".into(),
                min_moisture: 0.3,
                target_moisture: 0.5,
                pulse_sec: 30,
                soak_min: 20,
                max_open_sec_per_day: 180,
                max_pulses_per_day: 6,
                stale_timeout_min: 30,
                valve_gpio_pin: 17,
            })
            .await
            .unwrap();

        let app = router(state);
        let req = Request::builder()
            .uri("/api/zones")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["zone_id"], "z1");
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = router(test_state().await);
        let req = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
