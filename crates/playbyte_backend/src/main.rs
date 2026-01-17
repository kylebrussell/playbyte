use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use playbyte_types::ByteMetadata;
use serde::Serialize;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone, Default)]
struct AppState {
    bytes: Arc<RwLock<Vec<ByteMetadata>>>,
}

#[derive(Serialize)]
struct FeedResponse {
    items: Vec<ByteMetadata>,
}

#[tokio::main]
async fn main() {
    let state = AppState::default();

    let app = Router::new()
        .route("/health", get(health))
        .route("/feed", get(get_feed))
        .route("/bytes/:id", get(get_byte))
        .route("/bytes/:id/state", get(get_byte_state))
        .route("/bytes/:id/thumbnail", get(get_byte_thumbnail))
        .route("/bytes", post(post_byte))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("Playbyte backend listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server error");
}

async fn health() -> &'static str {
    "ok"
}

async fn get_feed(State(state): State<AppState>) -> Json<FeedResponse> {
    let bytes = state.bytes.read().await;
    Json(FeedResponse {
        items: bytes.clone(),
    })
}

async fn get_byte(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ByteMetadata>, StatusCode> {
    let bytes = state.bytes.read().await;
    let item = bytes.iter().find(|item| item.byte_id == id).cloned();
    match item {
        Some(item) => Ok(Json(item)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn post_byte() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

async fn get_byte_state(Path(_id): Path<String>) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

async fn get_byte_thumbnail(Path(_id): Path<String>) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
