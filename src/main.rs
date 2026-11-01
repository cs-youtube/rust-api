//! A small REST API, written the idiomatic way with axum and serde.
//! cs-youtube/go-api has the same endpoints and metrics, so the two can be compared under load.
use std::{sync::Arc, time::Instant};

use axum::{
    extract::{MatchedPath, Path, Query, Request, State},
    http::{header::CONTENT_TYPE, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use prometheus::{Encoder, HistogramOpts, HistogramVec, IntGauge, Registry, TextEncoder};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Item {
    #[serde(default)]
    id: u64,
    name: String,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    tags: Vec<String>,
}

/// The model: created once at start and only read after that.
fn new_items(count: u64) -> Vec<Item> {
    (0..count)
        .map(|id| Item {
            id,
            name: format!("item-{id}"),
            price: id as f64 * 1.5,
            active: id % 2 == 0,
            tags: vec!["alpha".into(), "beta".into(), "gamma".into()],
        })
        .collect()
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<usize>,
}

async fn list_items(State(state): State<AppState>, Query(params): Query<ListParams>) -> Response {
    let limit = params.limit.filter(|n| *n <= state.items.len()).unwrap_or(50);
    Json(&state.items[..limit]).into_response()
}

async fn get_item(
    State(state): State<AppState>,
    id: Result<Path<u64>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return (StatusCode::BAD_REQUEST, "bad id").into_response();
    };
    match state.items.get(id as usize) {
        Some(item) => Json(item).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn create_item(item: Result<Json<Item>, axum::extract::rejection::JsonRejection>) -> Response {
    match item {
        Ok(Json(item)) if !item.name.is_empty() => (StatusCode::CREATED, Json(item)).into_response(),
        _ => (StatusCode::BAD_REQUEST, "bad item").into_response(),
    }
}

#[derive(Clone)]
struct AppState {
    items: Arc<Vec<Item>>,
    metrics: Metrics,
}

#[derive(Clone)]
struct Metrics {
    registry: Registry,
    requests: HistogramVec,
    in_flight: IntGauge,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();
        let requests = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "Time from receiving a request to finishing its response.",
            )
            .buckets(vec![
                0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0,
            ]),
            &["method", "route", "code"],
        )
        .unwrap();
        let in_flight = IntGauge::new("http_requests_in_flight", "Requests being handled right now.").unwrap();
        registry.register(Box::new(requests.clone())).unwrap();
        registry.register(Box::new(in_flight.clone())).unwrap();
        #[cfg(target_os = "linux")]
        registry
            .register(Box::new(prometheus::process_collector::ProcessCollector::for_self()))
            .unwrap();
        Self { registry, requests, in_flight }
    }
}

async fn instrument(State(state): State<AppState>, route: MatchedPath, request: Request, next: Next) -> Response {
    let metrics = &state.metrics;
    let method = request.method().clone();
    metrics.in_flight.inc();
    let start = Instant::now();
    let response = next.run(request).await;
    metrics
        .requests
        .with_label_values(&[method.as_str(), route.as_str(), response.status().as_str()])
        .observe(start.elapsed().as_secs_f64());
    metrics.in_flight.dec();
    response
}

async fn serve_metrics(State(state): State<AppState>) -> Response {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    encoder.encode(&state.metrics.registry.gather(), &mut buffer).unwrap();
    // Prometheus refuses to scrape a response without the right content type.
    ([(CONTENT_TYPE, encoder.format_type().to_owned())], buffer).into_response()
}

fn app() -> Router {
    let state = AppState { items: Arc::new(new_items(1000)), metrics: Metrics::new() };
    Router::new()
        .route("/items", get(list_items).post(create_item))
        .route("/items/{id}", get(get_item))
        .route_layer(middleware::from_fn_with_state(state.clone(), instrument))
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/metrics", get(serve_metrics))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app()).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn request(method: &str, uri: &str, body: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        (status, response.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    #[tokio::test]
    async fn list_items_honours_limit() {
        let (_, body) = request("GET", "/items?limit=3", "").await;
        assert_eq!(serde_json::from_slice::<Vec<Item>>(&body).unwrap().len(), 3);
    }

    #[tokio::test]
    async fn get_item_by_id() {
        let (_, body) = request("GET", "/items/7", "").await;
        assert_eq!(serde_json::from_slice::<Item>(&body).unwrap().name, "item-7");
        assert_eq!(request("GET", "/items/x", "").await.0, StatusCode::BAD_REQUEST);
        assert_eq!(request("GET", "/items/1000", "").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn metrics_have_the_content_type_prometheus_needs() {
        let request = Request::builder().uri("/metrics").body(Body::empty()).unwrap();
        let response = app().oneshot(request).await.unwrap();
        let content_type = response.headers()[CONTENT_TYPE].to_str().unwrap();
        assert!(content_type.starts_with("text/plain"), "{content_type}");
    }

    #[tokio::test]
    async fn create_item_validates() {
        assert_eq!(request("POST", "/items", r#"{"name":"a"}"#).await.0, StatusCode::CREATED);
        assert_eq!(request("POST", "/items", r#"{"name":""}"#).await.0, StatusCode::BAD_REQUEST);
    }
}
