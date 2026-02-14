use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use std::env;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use crate::db::{Db, SensorConfig, ZoneConfig};
use crate::state::SharedState;

// this is built by the ui/package.json build script into the dist/index.html file
const INDEX_HTML: &str = include_str!("ui/dist/index.html");

// ---------------------------------------------------------------------------
// Composite app state shared across all handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppState {
    pub shared: SharedState,
    pub db: Db,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

enum ApiError {
    NotFound(String),
    Validation(Vec<String>),
    Conflict(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, body) = match self {
            Self::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                serde_json::json!({"error": "not_found", "message": msg}),
            ),
            Self::Validation(msgs) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({"error": "validation", "messages": msgs}),
            ),
            Self::Conflict(msg) => (
                StatusCode::CONFLICT,
                serde_json::json!({"error": "conflict", "message": msg}),
            ),
            Self::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": "internal", "message": msg}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    eprintln!("internal error: {e:#}");
    ApiError::Internal(e.to_string())
}

fn db_delete_err(e: anyhow::Error) -> ApiError {
    let full = format!("{e:#}");
    if full.contains("FOREIGN KEY constraint failed") {
        ApiError::Conflict("cannot delete: referenced by other records".into())
    } else {
        internal(e)
    }
}

// ---------------------------------------------------------------------------
// Request / query types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ZonePayload {
    name: String,
    min_moisture: f32,
    target_moisture: f32,
    pulse_sec: i64,
    soak_min: i64,
    max_open_sec_per_day: i64,
    max_pulses_per_day: i64,
    stale_timeout_min: i64,
    valve_gpio_pin: i64,
}

#[derive(Deserialize)]
struct SensorPayload {
    node_id: String,
    zone_id: String,
    raw_dry: i64,
    raw_wet: i64,
}

#[derive(Deserialize)]
struct ReadingsQuery {
    sensor_id: Option<String>,
    zone_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct EventsQuery {
    zone_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct CountersQuery {
    day: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_zone(p: &ZonePayload) -> Result<(), ApiError> {
    let mut errs = Vec::new();
    if p.name.trim().is_empty() {
        errs.push("name must not be empty".into());
    }
    if !(0.0..=1.0).contains(&p.min_moisture) {
        errs.push("min_moisture must be in 0.0..=1.0".into());
    }
    if !(0.0..=1.0).contains(&p.target_moisture) {
        errs.push("target_moisture must be in 0.0..=1.0".into());
    }
    if p.target_moisture < p.min_moisture {
        errs.push("target_moisture must be >= min_moisture".into());
    }
    if p.pulse_sec <= 0 {
        errs.push("pulse_sec must be > 0".into());
    }
    if p.soak_min <= 0 {
        errs.push("soak_min must be > 0".into());
    }
    if p.max_open_sec_per_day <= 0 {
        errs.push("max_open_sec_per_day must be > 0".into());
    }
    if p.max_pulses_per_day <= 0 {
        errs.push("max_pulses_per_day must be > 0".into());
    }
    if p.stale_timeout_min <= 0 {
        errs.push("stale_timeout_min must be > 0".into());
    }
    if p.valve_gpio_pin < 0 {
        errs.push("valve_gpio_pin must be >= 0".into());
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errs))
    }
}

fn validate_sensor(p: &SensorPayload) -> Result<(), ApiError> {
    let mut errs = Vec::new();
    if p.node_id.trim().is_empty() {
        errs.push("node_id must not be empty".into());
    }
    if p.zone_id.trim().is_empty() {
        errs.push("zone_id must not be empty".into());
    }
    if p.raw_dry == p.raw_wet {
        errs.push("raw_dry and raw_wet must differ".into());
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errs))
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        // Zones
        .route("/api/zones", get(api_zones))
        .route(
            "/api/zones/{zone_id}",
            get(api_get_zone)
                .put(api_upsert_zone)
                .delete(api_delete_zone),
        )
        // Sensors
        .route("/api/sensors", get(api_sensors))
        .route(
            "/api/sensors/{sensor_id}",
            get(api_get_sensor)
                .put(api_upsert_sensor)
                .delete(api_delete_sensor),
        )
        // Readings / events / counters (read-only)
        .route("/api/readings", get(api_readings))
        .route("/api/watering-events", get(api_watering_events))
        .route("/api/counters/{zone_id}", get(api_counters))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers — static
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Handlers — zones
// ---------------------------------------------------------------------------

async fn api_zones(
    State(state): State<AppState>,
) -> Result<Json<Vec<ZoneConfig>>, ApiError> {
    state.db.load_zones().await.map(Json).map_err(internal)
}

async fn api_get_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<Json<ZoneConfig>, ApiError> {
    state
        .db
        .get_zone(&zone_id)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("zone '{zone_id}' not found")))
}

async fn api_upsert_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(payload): Json<ZonePayload>,
) -> Result<Json<ZoneConfig>, ApiError> {
    validate_zone(&payload)?;

    let config = ZoneConfig {
        zone_id,
        name: payload.name,
        min_moisture: payload.min_moisture,
        target_moisture: payload.target_moisture,
        pulse_sec: payload.pulse_sec,
        soak_min: payload.soak_min,
        max_open_sec_per_day: payload.max_open_sec_per_day,
        max_pulses_per_day: payload.max_pulses_per_day,
        stale_timeout_min: payload.stale_timeout_min,
        valve_gpio_pin: payload.valve_gpio_pin,
    };

    state.db.upsert_zone(&config).await.map_err(internal)?;
    Ok(Json(config))
}

async fn api_delete_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let deleted = state
        .db
        .delete_zone(&zone_id)
        .await
        .map_err(db_delete_err)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!(
            "zone '{zone_id}' not found"
        )))
    }
}

// ---------------------------------------------------------------------------
// Handlers — sensors
// ---------------------------------------------------------------------------

async fn api_sensors(
    State(state): State<AppState>,
) -> Result<Json<Vec<SensorConfig>>, ApiError> {
    state.db.load_sensors().await.map(Json).map_err(internal)
}

async fn api_get_sensor(
    State(state): State<AppState>,
    Path(sensor_id): Path<String>,
) -> Result<Json<SensorConfig>, ApiError> {
    state
        .db
        .get_sensor(&sensor_id)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("sensor '{sensor_id}' not found")))
}

async fn api_upsert_sensor(
    State(state): State<AppState>,
    Path(sensor_id): Path<String>,
    Json(payload): Json<SensorPayload>,
) -> Result<Json<SensorConfig>, ApiError> {
    validate_sensor(&payload)?;

    // Verify the referenced zone exists.
    let zone = state
        .db
        .get_zone(&payload.zone_id)
        .await
        .map_err(internal)?;
    if zone.is_none() {
        return Err(ApiError::Validation(vec![format!(
            "zone '{}' does not exist",
            payload.zone_id
        )]));
    }

    let config = SensorConfig {
        sensor_id,
        node_id: payload.node_id,
        zone_id: payload.zone_id,
        raw_dry: payload.raw_dry,
        raw_wet: payload.raw_wet,
    };

    state.db.upsert_sensor(&config).await.map_err(internal)?;
    Ok(Json(config))
}

async fn api_delete_sensor(
    State(state): State<AppState>,
    Path(sensor_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let deleted = state
        .db
        .delete_sensor(&sensor_id)
        .await
        .map_err(db_delete_err)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!(
            "sensor '{sensor_id}' not found"
        )))
    }
}

// ---------------------------------------------------------------------------
// Handlers — readings (read-only)
// ---------------------------------------------------------------------------

async fn api_readings(
    State(state): State<AppState>,
    Query(q): Query<ReadingsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);

    let rows = state
        .db
        .list_readings(q.sensor_id.as_deref(), q.zone_id.as_deref(), limit, offset)
        .await
        .map_err(internal)?;

    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Handlers — watering events (read-only)
// ---------------------------------------------------------------------------

async fn api_watering_events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);

    let rows = state
        .db
        .list_watering_events(q.zone_id.as_deref(), limit, offset)
        .await
        .map_err(internal)?;

    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Handlers — daily counters (read-only)
// ---------------------------------------------------------------------------

async fn api_counters(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Query(q): Query<CountersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let day = q.day.unwrap_or_else(Db::today_yyyy_mm_dd);
    let counters = state
        .db
        .get_daily_counters(&day, &zone_id)
        .await
        .map_err(internal)?;
    Ok(Json(counters))
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

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build an AppState backed by an in-memory SQLite DB for testing.
    async fn test_state() -> AppState {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();

        let zones = vec![("zone1".to_string(), 17), ("zone2".to_string(), 27)];
        let shared = Arc::new(RwLock::new(SystemState::new(&zones)));

        AppState { shared, db }
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn put_json(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn delete_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn sample_zone_json() -> serde_json::Value {
        serde_json::json!({
            "name": "Front Lawn",
            "min_moisture": 0.3,
            "target_moisture": 0.5,
            "pulse_sec": 30,
            "soak_min": 20,
            "max_open_sec_per_day": 180,
            "max_pulses_per_day": 6,
            "stale_timeout_min": 30,
            "valve_gpio_pin": 17
        })
    }

    fn sample_sensor_json(zone_id: &str) -> serde_json::Value {
        serde_json::json!({
            "node_id": "node-a",
            "zone_id": zone_id,
            "raw_dry": 30000,
            "raw_wet": 10000
        })
    }

    // -----------------------------------------------------------------------
    // Static routes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn index_returns_html() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/")).await.unwrap();

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
        let resp = app.oneshot(get_req("/api/status")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;

        assert!(json["uptime_secs"].is_u64());
        assert!(json["mqtt_connected"].is_boolean());
        assert!(json["nodes"].is_object());
        assert!(json["zones"].is_object());
        assert!(json["events"].is_array());
        assert!(json["zones"]["zone1"].is_object());
        assert!(json["zones"]["zone2"].is_object());
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/nonexistent")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // Zones — list
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn api_zones_returns_empty_array_when_no_zones() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/api/zones")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json.is_array());
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn api_zones_returns_inserted_zone() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&ZoneConfig {
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
        let resp = app.oneshot(get_req("/api/zones")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["zone_id"], "z1");
    }

    // -----------------------------------------------------------------------
    // Zones — CRUD
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_zone_creates_and_returns_zone() {
        let app = router(test_state().await);
        let resp = app
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["zone_id"], "z1");
        assert_eq!(json["name"], "Front Lawn");
    }

    #[tokio::test]
    async fn put_zone_then_get_returns_same() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let resp = app.oneshot(get_req("/api/zones/z1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["zone_id"], "z1");
        assert_eq!(json["name"], "Front Lawn");
        assert_eq!(json["pulse_sec"], 30);
    }

    #[tokio::test]
    async fn put_zone_upsert_updates_existing() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let mut updated = sample_zone_json();
        updated["name"] = serde_json::json!("Back Yard");
        app.clone()
            .oneshot(put_json("/api/zones/z1", updated))
            .await
            .unwrap();

        let resp = app.oneshot(get_req("/api/zones/z1")).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["name"], "Back Yard");
    }

    #[tokio::test]
    async fn get_zone_missing_returns_404() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/api/zones/nope")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let json = body_json(resp).await;
        assert_eq!(json["error"], "not_found");
    }

    #[tokio::test]
    async fn delete_zone_removes_it() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(delete_req("/api/zones/z1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app.oneshot(get_req("/api/zones/z1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_zone_missing_returns_404() {
        let app = router(test_state().await);
        let resp = app.oneshot(delete_req("/api/zones/nope")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_zone_with_sensors_returns_409() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();
        app.clone()
            .oneshot(put_json("/api/sensors/s1", sample_sensor_json("z1")))
            .await
            .unwrap();

        let resp = app.oneshot(delete_req("/api/zones/z1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // Zone validation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_zone_bad_moisture_returns_422() {
        let app = router(test_state().await);
        let mut bad = sample_zone_json();
        bad["min_moisture"] = serde_json::json!(1.5);
        bad["target_moisture"] = serde_json::json!(-0.1);

        let resp = app
            .oneshot(put_json("/api/zones/z1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let json = body_json(resp).await;
        assert_eq!(json["error"], "validation");
        let msgs = json["messages"].as_array().unwrap();
        assert!(msgs.len() >= 2);
    }

    #[tokio::test]
    async fn put_zone_target_below_min_returns_422() {
        let app = router(test_state().await);
        let mut bad = sample_zone_json();
        bad["min_moisture"] = serde_json::json!(0.6);
        bad["target_moisture"] = serde_json::json!(0.3);

        let resp = app
            .oneshot(put_json("/api/zones/z1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn put_zone_empty_name_returns_422() {
        let app = router(test_state().await);
        let mut bad = sample_zone_json();
        bad["name"] = serde_json::json!("");

        let resp = app
            .oneshot(put_json("/api/zones/z1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn put_zone_negative_pulse_sec_returns_422() {
        let app = router(test_state().await);
        let mut bad = sample_zone_json();
        bad["pulse_sec"] = serde_json::json!(-1);

        let resp = app
            .oneshot(put_json("/api/zones/z1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // -----------------------------------------------------------------------
    // Sensors — CRUD
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_sensor_creates_and_returns_sensor() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let resp = app
            .oneshot(put_json("/api/sensors/s1", sample_sensor_json("z1")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["sensor_id"], "s1");
        assert_eq!(json["zone_id"], "z1");
    }

    #[tokio::test]
    async fn get_sensors_list() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();
        app.clone()
            .oneshot(put_json("/api/sensors/s1", sample_sensor_json("z1")))
            .await
            .unwrap();
        app.clone()
            .oneshot(put_json("/api/sensors/s2", sample_sensor_json("z1")))
            .await
            .unwrap();

        let resp = app.oneshot(get_req("/api/sensors")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_sensor_single() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();
        app.clone()
            .oneshot(put_json("/api/sensors/s1", sample_sensor_json("z1")))
            .await
            .unwrap();

        let resp = app.oneshot(get_req("/api/sensors/s1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["sensor_id"], "s1");
        assert_eq!(json["node_id"], "node-a");
    }

    #[tokio::test]
    async fn get_sensor_missing_returns_404() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/api/sensors/nope")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_sensor_removes_it() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();
        app.clone()
            .oneshot(put_json("/api/sensors/s1", sample_sensor_json("z1")))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(delete_req("/api/sensors/s1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app.oneshot(get_req("/api/sensors/s1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_sensor_missing_returns_404() {
        let app = router(test_state().await);
        let resp = app
            .oneshot(delete_req("/api/sensors/nope"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // Sensor validation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_sensor_nonexistent_zone_returns_422() {
        let app = router(test_state().await);
        let resp = app
            .oneshot(put_json(
                "/api/sensors/s1",
                sample_sensor_json("no-such-zone"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let json = body_json(resp).await;
        assert!(json["messages"][0]
            .as_str()
            .unwrap()
            .contains("no-such-zone"));
    }

    #[tokio::test]
    async fn put_sensor_equal_dry_wet_returns_422() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let bad = serde_json::json!({
            "node_id": "node-a",
            "zone_id": "z1",
            "raw_dry": 20000,
            "raw_wet": 20000
        });
        let resp = app
            .oneshot(put_json("/api/sensors/s1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn put_sensor_empty_node_id_returns_422() {
        let app = router(test_state().await);
        app.clone()
            .oneshot(put_json("/api/zones/z1", sample_zone_json()))
            .await
            .unwrap();

        let bad = serde_json::json!({
            "node_id": "",
            "zone_id": "z1",
            "raw_dry": 30000,
            "raw_wet": 10000
        });
        let resp = app
            .oneshot(put_json("/api/sensors/s1", bad))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // -----------------------------------------------------------------------
    // Readings (read-only)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn readings_empty_returns_empty_array() {
        let app = router(test_state().await);
        let resp = app.oneshot(get_req("/api/readings")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn readings_returns_inserted_data() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&ZoneConfig {
                zone_id: "z1".into(),
                name: "Z".into(),
                min_moisture: 0.2,
                target_moisture: 0.5,
                pulse_sec: 30,
                soak_min: 10,
                max_open_sec_per_day: 120,
                max_pulses_per_day: 4,
                stale_timeout_min: 30,
                valve_gpio_pin: 17,
            })
            .await
            .unwrap();
        state
            .db
            .upsert_sensor(&SensorConfig {
                sensor_id: "s1".into(),
                node_id: "n1".into(),
                zone_id: "z1".into(),
                raw_dry: 30000,
                raw_wet: 10000,
            })
            .await
            .unwrap();
        state
            .db
            .insert_reading(1000, "s1", 20000, 0.5)
            .await
            .unwrap();
        state
            .db
            .insert_reading(1001, "s1", 21000, 0.45)
            .await
            .unwrap();

        let app = router(state);

        // Filter by sensor_id
        let resp = app
            .clone()
            .oneshot(get_req("/api/readings?sensor_id=s1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 2);

        // Filter by zone_id
        let resp = app
            .clone()
            .oneshot(get_req("/api/readings?zone_id=z1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 2);

        // Limit
        let resp = app
            .clone()
            .oneshot(get_req("/api/readings?limit=1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);

        // Offset
        let resp = app
            .oneshot(get_req("/api/readings?limit=10&offset=1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Watering events (read-only)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn watering_events_empty_returns_empty_array() {
        let app = router(test_state().await);
        let resp = app
            .oneshot(get_req("/api/watering-events"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn watering_events_returns_inserted_data() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&ZoneConfig {
                zone_id: "z1".into(),
                name: "Z".into(),
                min_moisture: 0.2,
                target_moisture: 0.5,
                pulse_sec: 30,
                soak_min: 10,
                max_open_sec_per_day: 120,
                max_pulses_per_day: 4,
                stale_timeout_min: 30,
                valve_gpio_pin: 17,
            })
            .await
            .unwrap();
        state
            .db
            .insert_watering_event(1000, 1030, "z1", "dry", "ok")
            .await
            .unwrap();
        state
            .db
            .insert_watering_event(2000, 2030, "z1", "dry", "ok")
            .await
            .unwrap();

        let app = router(state);

        // All events
        let resp = app
            .clone()
            .oneshot(get_req("/api/watering-events"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 2);

        // Filter by zone_id
        let resp = app
            .clone()
            .oneshot(get_req("/api/watering-events?zone_id=z1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 2);

        // Limit + offset
        let resp = app
            .oneshot(get_req("/api/watering-events?limit=1&offset=1"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Daily counters (read-only)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn counters_returns_zeros_for_no_data() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&ZoneConfig {
                zone_id: "z1".into(),
                name: "Z".into(),
                min_moisture: 0.2,
                target_moisture: 0.5,
                pulse_sec: 30,
                soak_min: 10,
                max_open_sec_per_day: 120,
                max_pulses_per_day: 4,
                stale_timeout_min: 30,
                valve_gpio_pin: 17,
            })
            .await
            .unwrap();

        let app = router(state);
        let resp = app
            .oneshot(get_req("/api/counters/z1?day=2025-01-01"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["open_sec"], 0);
        assert_eq!(json["pulses"], 0);
    }

    #[tokio::test]
    async fn counters_returns_accumulated_data() {
        let state = test_state().await;
        state
            .db
            .upsert_zone(&ZoneConfig {
                zone_id: "z1".into(),
                name: "Z".into(),
                min_moisture: 0.2,
                target_moisture: 0.5,
                pulse_sec: 30,
                soak_min: 10,
                max_open_sec_per_day: 120,
                max_pulses_per_day: 4,
                stale_timeout_min: 30,
                valve_gpio_pin: 17,
            })
            .await
            .unwrap();
        state
            .db
            .add_pulse("2025-06-01", "z1", 1)
            .await
            .unwrap();
        state
            .db
            .add_open_seconds("2025-06-01", "z1", 30)
            .await
            .unwrap();

        let app = router(state);
        let resp = app
            .oneshot(get_req("/api/counters/z1?day=2025-06-01"))
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["pulses"], 1);
        assert_eq!(json["open_sec"], 30);
    }
}
