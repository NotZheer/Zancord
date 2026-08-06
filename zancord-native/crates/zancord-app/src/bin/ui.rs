//! Zancord desktop client (Phase 4.10 / Phase 6): renders the Slint main
//! window and hands control to the app orchestrator, which wires signaling,
//! mesh, and audio to the UI.
//!
//! Launch modes:
//! - No positional args: show the join screen (endpoint/room/display name,
//!   prefilled from `config.json`).
//! - `zancord-ui <ws-url> <room> <username>`: join immediately (scripts).
//!
//! Both accept `[--input <id>] [--output <id>]` device overrides.

use std::sync::Arc;

use anyhow::Context;
use slint::ComponentHandle;

use zancord_app::config::AppConfig;
use zancord_app::MainWindow;

fn usage() -> String {
    "usage: zancord-ui [<ws-url> <room> <username>] [--input <id>] [--output <id>]".to_string()
}

#[derive(Default)]
struct Args {
    ws_url: Option<String>,
    room: Option<String>,
    username: Option<String>,
    input_device: Option<String>,
    output_device: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        std::process::exit(0);
    }
    let mut args = Args::default();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--input" => {
                i += 1;
                args.input_device =
                    Some(raw.get(i).context("--input requires a device id")?.clone());
            }
            "--output" => {
                i += 1;
                args.output_device =
                    Some(raw.get(i).context("--output requires a device id")?.clone());
            }
            other if args.ws_url.is_none() => args.ws_url = Some(other.to_owned()),
            other if args.room.is_none() => args.room = Some(other.to_owned()),
            other if args.username.is_none() => args.username = Some(other.to_owned()),
            other => anyhow::bail!("unknown argument: {other}\n{}", usage()),
        }
        i += 1;
    }
    // Either all three join args, or none (join screen).
    let positional = [&args.ws_url, &args.room, &args.username];
    let count = positional.iter().filter(|v| v.is_some()).count();
    if count != 0 && count != 3 {
        anyhow::bail!("{}", usage());
    }
    Ok(args)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zancord_transport=debug,zancord_audio=info".into()),
        )
        .init();

    let args = parse_args()?;
    let config = AppConfig::load();

    // The orchestrator runs on this runtime (spawned per join); the UI thread
    // runs the Slint event loop.
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime")?,
    );

    let window = MainWindow::new()?;
    window.set_join_endpoint(
        args.ws_url
            .clone()
            .or_else(|| config.last_endpoint.clone())
            .unwrap_or_else(|| "ws://127.0.0.1:3000".to_string())
            .into(),
    );
    window.set_join_room(
        args.room
            .clone()
            .or_else(|| config.last_room.clone())
            .unwrap_or_else(|| "zancord-room".to_string())
            .into(),
    );
    window.set_join_username(
        args.username
            .clone()
            .or_else(|| config.username.clone())
            .unwrap_or_default()
            .into(),
    );

    let join_window = window.clone_strong();
    window.on_join_clicked(move |endpoint, room, username| {
        // Guard against double-clicks (the UI flips to the call view here).
        if join_window.get_in_call() {
            return;
        }
        let (endpoint, room, username) = (
            endpoint.trim().to_string(),
            room.trim().to_string(),
            username.trim().to_string(),
        );
        join_window.set_in_call(true);
        let window = join_window.as_weak();
        let runtime = Arc::clone(&runtime);
        let input_override = args.input_device.clone();
        let output_override = args.output_device.clone();
        runtime.spawn(async move {
            // Persistent config: prefer CLI flags, fall back to saved
            // preferences, then the host default devices.
            let mut config = AppConfig::load();
            let input_device = match input_override {
                Some(id) => Some(id),
                None => match config.input_device.clone() {
                    Some(id) => Some(id),
                    None => zancord_app::app::default_device(
                        zancord_audio::devices::list_input_devices().unwrap_or_default(),
                    ),
                },
            };
            let output_device = match output_override {
                Some(id) => Some(id),
                None => match config.output_device.clone() {
                    Some(id) => Some(id),
                    None => zancord_app::app::default_device(
                        zancord_audio::devices::list_output_devices().unwrap_or_default(),
                    ),
                },
            };

            // Remember the endpoint/room/username for the next launch.
            config.last_endpoint = Some(endpoint.clone());
            config.last_room = Some(room.clone());
            config.username = Some(username.clone());
            if let Err(err) = config.save() {
                tracing::warn!(error = %err, "failed to save config");
            }

            if let Err(err) = zancord_app::app::App::run(
                window.clone(),
                endpoint,
                room,
                username,
                input_device,
                output_device,
            )
            .await
            {
                tracing::error!(error = %err, "app orchestrator exited with an error");
                let _ = window.upgrade_in_event_loop(move |w| {
                    w.set_in_call(false);
                    w.set_join_error(format!("{err:#}").into());
                });
            }
        });
    });

    // CLI mode: skip the join screen and connect immediately (the callback
    // flips the view and spawns the orchestrator).
    if args.ws_url.is_some() {
        window.invoke_join_clicked(
            args.ws_url
                .as_deref()
                .unwrap_or_default()
                .to_string()
                .into(),
            args.room.as_deref().unwrap_or_default().to_string().into(),
            args.username
                .as_deref()
                .unwrap_or_default()
                .to_string()
                .into(),
        );
    }

    window.run()?;
    Ok(())
}
