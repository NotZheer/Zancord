//! Zancord desktop client (Phase 4.10): renders the Slint main window and
//! hands control to the app orchestrator, which wires signaling, mesh, and
//! audio to the UI.
//!
//! Usage: zancord-ui <ws-url> <room> <username> [--input <id>] [--output <id>]

use anyhow::Context;
use slint::ComponentHandle;

use zancord_app::MainWindow;

fn usage() -> String {
    "usage: zancord-ui <ws-url> <room> <username> [--input <id>] [--output <id>]".to_string()
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
    if args.ws_url.is_none() || args.room.is_none() || args.username.is_none() {
        anyhow::bail!("{}", usage());
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zancord_transport=debug,zancord_audio=info".into()),
        )
        .init();

    let args = parse_args()?;

    // Resolve default devices when not overridden (None disables that direction).
    let input_device = match args.input_device {
        Some(id) => Some(id),
        None => zancord_app::app::default_device(zancord_audio::devices::list_input_devices()?),
    };
    let output_device = match args.output_device {
        Some(id) => Some(id),
        None => zancord_app::app::default_device(zancord_audio::devices::list_output_devices()?),
    };

    let window = MainWindow::new()?;
    let weak = window.as_weak();

    // The orchestrator runs on a tokio worker; UI mutations hop back to the
    // UI thread via `upgrade_in_event_loop`.
    let ws_url = args.ws_url.expect("parsed");
    let room = args.room.expect("parsed");
    let username = args.username.expect("parsed");
    tokio::spawn(zancord_app::app::App::run(
        weak,
        ws_url,
        room,
        username,
        input_device,
        output_device,
    ));

    window.run()?;
    Ok(())
}
