//! Zancord signaling server library.
//!
//! Axum + WebSocket signaling relay: ephemeral in-memory rooms, zero database.
//! Binary entrypoint in `main.rs`; this library exposes the room manager and
//! WS message handler for unit/integration testing.

#![deny(clippy::all)]

pub mod handler;
pub mod rate_limit;
pub mod room;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;

/// Builds the HTTP router with the `GET /ws/:room_id` upgrade route.
pub fn app(manager: Arc<room::RoomManager>) -> Router {
    Router::new()
        .route("/ws/:room_id", get(handler::ws_handler))
        .with_state(manager)
}

/// Serves [`app`] on `listener` until the listener is dropped. Exposed so the
/// binary and integration tests run the app without axum ceremony.
pub async fn serve(
    listener: tokio::net::TcpListener,
    manager: Arc<room::RoomManager>,
) -> anyhow::Result<()> {
    axum::serve(listener, app(manager)).await?;
    Ok(())
}
