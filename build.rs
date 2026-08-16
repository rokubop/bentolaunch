use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Embed the manifest directly via link.exe rather than pulling in a resource
    // crate. Gives us `asInvoker` (safety rule 1) and PerMonitorV2 DPI awareness.
    let manifest = root.join("dashpick.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );

    // The icon needs a real resource, which needs rc.exe. A machine without it
    // still builds; it just gets the stock exe icon.
    let icon = root.join("assets").join("dashpick.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy());
    if let Err(e) = res.compile() {
        println!("cargo:warning=no exe icon ({e}); rc.exe was not usable");
    }
}
