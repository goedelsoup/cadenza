// Tauri's build script generates the runtime context (config, capabilities,
// asset embedding) from `tauri.conf.json`. It must run before `main.rs` is
// compiled, hence its presence as a build script rather than a normal dep.
fn main() {
    tauri_build::build()
}
