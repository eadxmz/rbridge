fn main() {
    println!("cargo:rerun-if-changed=assets/rbridge.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/rbridge.ico");
        res.set("ProductName", "RBridge Control Center");
        res.set("FileDescription", "RBridge — Modern Radeon Driver Patcher & Overclocker");
        res.compile().expect("Failed to compile Windows icon resource");
    }
}
