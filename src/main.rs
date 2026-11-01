//! A small REST API, written the idiomatic way with axum and serde.
//! cs-youtube/go-api has the same endpoints and metrics, so the two can be compared under load.
use std::time::Instant;

use axum::{
    extract::{MatchedPath, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use prometheus::{Encoder, HistogramOpts, HistogramVec, IntGauge, Registry, TextEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

fn new_item(id: u64) -> Item {
    Item {
        id,
        name: format!("item-{id}"),
        price: id as f64 * 1.5,
        active: id % 2 == 0,
        tags: vec!["alpha".into(), "beta".into(), "gamma".into()],
    }
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<usize>,
}

async fn list_items(Query(params): Query<ListParams>) -> Json<Vec<Item>> {
    let limit = params.limit.filter(|n| *n <= 1000).unwrap_or(50);
    Json((0..limit as u64).map(new_item).collect())
}

async fn get_item(id: Result<Path<u64>, axum::extract::rejection::PathRejection>) -> Response {
    match id {
        Ok(Path(id)) => Json(new_item(id)).into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "bad id").into_response(),
    }
}

async fn create_item(item: Result<Json<Item>, axum::extract::rejection::JsonRejection>) -> Response {
    match item {
        Ok(Json(item)) if !item.name.is_empty() => (StatusCode::CREATED, Json(item)).into_response(),
        _ => (StatusCode::BAD_REQUEST, "bad item").into_response(),
    }
}

#[derive(Deserialize)]
struct WorkParams {
    n: Option<u64>,
}

/// Burns CPU for a fixed amount of work: n rounds of SHA-256.
async fn work(Query(params): Query<WorkParams>) -> String {
    let mut sum = Sha256::digest(b"");
    for _ in 0..params.n.filter(|n| *n <= 10_000_000).unwrap_or(10_000) {
        sum = Sha256::digest(sum);
    }
    format!("{:02x}{:02x}{:02x}{:02x}\n", sum[0], sum[1], sum[2], sum[3])
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

async fn instrument(State(metrics): State<Metrics>, route: MatchedPath, request: Request, next: Next) -> Response {
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

async fn serve_metrics(State(metrics): State<Metrics>) -> Vec<u8> {
    let mut buffer = Vec::new();
    TextEncoder::new().encode(&metrics.registry.gather(), &mut buffer).unwrap();
    buffer
}

fn app(metrics: Metrics) -> Router {
    Router::new()
        .route("/items", get(list_items).post(create_item))
        .route("/items/{id}", get(get_item))
        .route("/work", get(work))
        .route_layer(middleware::from_fn_with_state(metrics.clone(), instrument))
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/metrics", get(serve_metrics))
        .with_state(metrics)
}

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app(Metrics::new())).await.unwrap();
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
        let response = app(Metrics::new()).oneshot(request).await.unwrap();
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
    }

    #[tokio::test]
    async fn create_item_validates() {
        assert_eq!(request("POST", "/items", r#"{"name":"a"}"#).await.0, StatusCode::CREATED);
        assert_eq!(request("POST", "/items", r#"{"name":""}"#).await.0, StatusCode::BAD_REQUEST);
    }
}
