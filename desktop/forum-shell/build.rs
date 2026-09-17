use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let product_path = manifest.join("../../profiles/ai-vision-forum/product.json");
    println!("cargo:rerun-if-changed={}", product_path.display());
    println!("cargo:rerun-if-changed=tauri.conf.json");
    let product: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(product_path).expect("Forum build identity must exist"),
    )
    .expect("Forum build identity must be valid JSON");
    let tauri: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest.join("tauri.conf.json")).unwrap())
            .expect("Tauri configuration must be valid JSON");
    assert_eq!(tauri["identifier"], product["build_identity"]["bundle_id"]);
    assert_eq!(
        tauri["productName"],
        product["build_identity"]["product_name"]
    );
    assert_eq!(tauri["version"], product["development_version"]);
    assert_eq!(
        env::var("CARGO_PKG_VERSION").unwrap(),
        product["development_version"].as_str().unwrap()
    );
    assert_eq!(product["access"]["commercial_login"], false);
    assert_eq!(product["distribution"]["updater"]["enabled"], false);
    for (name, key) in [
        ("FORUM_AGENT_PRODUCT_NAME", "product_name"),
        ("FORUM_AGENT_DATA_DIRECTORY_NAME", "data_directory_name"),
    ] {
        println!(
            "cargo:rustc-env={name}={}",
            product["build_identity"][key].as_str().unwrap()
        );
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // ScreenCaptureKit's Swift runtime must resolve in installed builds,
        // without requiring the user to set DYLD_LIBRARY_PATH.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build()
}
