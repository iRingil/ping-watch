//! Build script: embeds the app icon into the Windows executable (shown by Explorer and the taskbar).

/// Compiles `assets/icon.ico` into the executable's resources.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    winresource::WindowsResource::new().set_icon("assets/icon.ico").compile()
}
