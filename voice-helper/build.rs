fn main() {
    // The sherpa-onnx libraries `npm run runtime` extracts, linked as set in .cargo/config.toml.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-search=native={manifest}/target/sherpa-onnx/lib");
}
