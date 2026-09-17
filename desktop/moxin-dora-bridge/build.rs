fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // ScreenCaptureKit's Swift bridge references system Swift libraries through
    // @rpath. Give this crate's test binaries the system runtime search path;
    // final application binaries must declare their own equivalent rpath.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
