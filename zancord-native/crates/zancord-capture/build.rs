//! Adds the Swift runtime library search path for Command-Line-Tools-only
//! installs, which `apple-metal`'s build.rs misses (it only emits the full
//! Xcode layout). Without this, linking Swift-bridged static libs fails:
//!
//! ```text
//! ld: symbol(s) not found for architecture arm64
//!     __swift_FORCE_LOAD_$_swiftCompatibility56
//! ```

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    if std::env::var("DOCS_RS").is_ok() {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let Ok(out) = Command::new("xcode-select").arg("-p").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let dev_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !dev_dir.ends_with("CommandLineTools") {
        // Full Xcode layout — apple-metal's build.rs already emits this path.
        return;
    }

    let lib_dir = format!("{dev_dir}/usr/lib/swift/macosx");
    if Path::new(&lib_dir).is_dir() {
        println!("cargo:rustc-link-search=native={lib_dir}");
    }
}
