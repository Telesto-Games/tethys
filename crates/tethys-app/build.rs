//! Embeds the application icon and version info in tethys.exe.
//!
//! GPUI loads resource #1 as the window icon; Explorer and the taskbar use it too.
//! Regenerate the .ico with `cargo run --manifest-path tools/make-icon/Cargo.toml`.

fn main() {
    println!("cargo::rerun-if-changed=../../assets/icon/tethys.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("../../assets/icon/tethys.ico", "1")
            .set("ProductName", "Tethys")
            .set(
                "FileDescription",
                "Tethys — an LLM-first IDE for Unreal projects",
            )
            .set("CompanyName", "Telesto Games");
        res.compile().expect("embedding the Windows icon resource");
    }
}
