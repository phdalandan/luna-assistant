fn main() {
    // Tests locate the bundled llama-server by its target-specific file name.
    println!(
        "cargo:rustc-env=LUNA_TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    // The sherpa-onnx libraries `npm run runtime` extracts, linked as set in .cargo/config.toml.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-search=native={manifest}/target/sherpa-onnx/lib");
    tauri_build::build()
}
