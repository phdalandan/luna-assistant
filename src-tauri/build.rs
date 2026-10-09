fn main() {
    // Tests locate the bundled llama-server by its target-specific file name.
    println!(
        "cargo:rustc-env=LUNA_TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    tauri_build::build()
}
