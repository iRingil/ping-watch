//! Build script: embeds the app icon and the user-facing name "Ping Watch" into the Windows executable (shown by
//! Explorer, the taskbar and the file's Properties → Details).

/// Compiles `assets/icon.ico` and the version info into the executable's resources.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    winresource::WindowsResource::new()
        .set_icon("assets/icon.ico")
        .set("ProductName", "Ping Watch")
        .set("FileDescription", "Ping Watch")
        .compile()
}
