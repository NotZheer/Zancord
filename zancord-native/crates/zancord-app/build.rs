//! Compiles the Slint UI (`ui/`) into Rust via `slint-build`.

fn main() {
    println!("cargo:rerun-if-changed=ui");
    slint_build::compile("ui/main_window.slint").expect("Slint UI failed to compile");
}
