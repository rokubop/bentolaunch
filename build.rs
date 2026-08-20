use std::path::PathBuf;

fn main() {
    // Read at run time, not `env!`. Baking the path in survives a folder rename
    // as a stale absolute path and the link fails on a directory that is gone.
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    // Embed the manifest directly via link.exe rather than pulling in a resource
    // crate. Gives us `asInvoker` (safety rule 1) and PerMonitorV2 DPI awareness.
    let manifest = root.join("bentopick.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );

    // The icon needs a real resource, which needs rc.exe. A machine without it
    // still builds; it just gets the stock exe icon.
    let icon = root.join("assets").join("bentopick.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy());
    if let Err(e) = res.compile() {
        println!("cargo:warning=no exe icon ({e}); rc.exe was not usable");
    }
}
