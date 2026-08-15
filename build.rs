use std::path::PathBuf;

fn main() {
    // Embed the manifest directly via link.exe rather than pulling in a resource
    // crate. Gives us `asInvoker` (safety rule 1) and PerMonitorV2 DPI awareness.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("flick.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
