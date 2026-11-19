fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Require UAC elevation on the interactive CLI only;
        // rbridge-daemon.exe runs as SYSTEM via Task Scheduler with no window.
        println!("cargo:rustc-link-arg-bin=rbridge=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=rbridge=/MANIFESTUAC:level='requireAdministrator'");
    }
}
