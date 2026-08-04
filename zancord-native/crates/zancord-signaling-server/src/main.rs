//! Zancord signaling server binary.
//!
//! Plain WebSocket on `0.0.0.0:3000`. Best-effort TLS on `0.0.0.0:3443` via
//! axum-server + rustls, enabled only when `cert.pem` / `key.pem` exist next
//! to the working directory; otherwise a warning is logged and plain WS only.

#![deny(clippy::all)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use axum_server::tls_rustls::RustlsConfig;
use tracing::{info, warn};

use zancord_signaling_server::{app, room::RoomManager, serve};

const DEFAULT_PORT: u16 = 3000;
const TLS_ADDR: &str = "0.0.0.0:3443";
const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zancord_signaling_server=debug".into()),
        )
        .init();

    let manager = Arc::new(RoomManager::new());

    // ZANCORD_PORT overrides the plain-WS port (useful when another service
    // already owns 3000).
    let port = std::env::var("ZANCORD_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let plain_addr = format!("0.0.0.0:{port}");

    let listener = tokio::net::TcpListener::bind(&plain_addr)
        .await
        .with_context(|| format!("failed to bind {plain_addr}"))?;
    info!(target: "zancord_signaling_server", addr = %plain_addr, "signaling server listening (plain WS)");
    let plain_manager = manager.clone();
    tokio::spawn(async move {
        if let Err(e) = serve(listener, plain_manager).await {
            warn!(target: "zancord_signaling_server", error = %e, "plain WS server stopped");
        }
    });

    if Path::new(CERT_FILE).exists() && Path::new(KEY_FILE).exists() {
        match RustlsConfig::from_pem_file(CERT_FILE, KEY_FILE).await {
            Ok(config) => {
                let addr: SocketAddr = TLS_ADDR
                    .parse()
                    .with_context(|| format!("invalid TLS addr {TLS_ADDR}"))?;
                info!(target: "zancord_signaling_server", addr = %TLS_ADDR, "signaling server listening (WSS/TLS)");
                if let Err(e) = axum_server::bind_rustls(addr, config)
                    .serve(app(manager).into_make_service())
                    .await
                {
                    warn!(target: "zancord_signaling_server", error = %e, "TLS server stopped; plain WS continues");
                }
            }
            Err(e) => {
                warn!(target: "zancord_signaling_server", error = %e, "failed to load TLS certs; serving plain WS only")
            }
        }
    } else {
        warn!(target: "zancord_signaling_server", "no {CERT_FILE}/{KEY_FILE} next to the working directory; TLS on {TLS_ADDR} disabled");
    }

    // The plain server runs in a spawned task; keep the process alive.
    std::future::pending::<()>().await;
    Ok(())
}
