//! Build script: embeds the app icon and the user-facing name "Ping Watch" into the Windows executable (shown by
//! Explorer, the taskbar and the file's Properties → Details), and has Windows load its DLLs only from System32.

/// Compiles `assets/icon.ico` and the version info into the executable's resources.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // The DLLs the executable imports are looked for in System32 only (`LOAD_LIBRARY_SEARCH_SYSTEM32`), not
    // beside it: the folder may be writable by any program of the user, and Ping Watch may run as administrator.
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    winresource::WindowsResource::new()
        .set_icon("assets/icon.ico")
        .set("ProductName", "Ping Watch")
        .set("FileDescription", "Ping Watch")
        .compile()
}
