//! Build script: on Windows, embed the app icon in `nimbus.exe`, so
//! Explorer and the taskbar show it. Other platforms get their icon from
//! the app bundle (macOS) or a `.desktop` file (Linux) instead.
//!
//! Only when building on Windows for Windows: the resource compiler that
//! embeds it (`rc.exe`, from the MSVC tools) is not available elsewhere,
//! and a cross-target lint check from another system has no need of it.

fn main() {
    println!("cargo::rerun-if-changed=../../assets/icon/nimbus.rc");
    println!("cargo::rerun-if-changed=../../assets/icon/nimbus.ico");
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    if cfg!(windows)
        && windows_target
        && let Err(e) = embed_resource::compile("../../assets/icon/nimbus.rc", embed_resource::NONE)
            .manifest_optional()
    {
        println!("cargo::warning=app icon not embedded: {e}");
    }
}
