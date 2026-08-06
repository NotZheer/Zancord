//! Zancord app library: the UI orchestrator (Phase 4.10) that wires the Slint
//! main window to signaling, mesh, and audio. The `zancord-app` bin stays as
//! the headless CLI harness; `zancord-ui` is the desktop client.

#![deny(clippy::all)]

slint::include_modules!();

pub mod app;
pub mod camera;
pub mod config;
pub mod screen_share;
