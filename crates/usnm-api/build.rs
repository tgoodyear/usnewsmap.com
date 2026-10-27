fn main() {
    for path in [
        "src",
        "build.rs",
        "Cargo.toml",
        "../usnm-core/src",
        "../usnm-search/src",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    // Docker can reuse the web build for an API-only deployment. Record API
    // compilation provenance too, so old replicas cannot undo newer renderers.
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("build clock after epoch")
        .as_micros();
    println!("cargo:rustc-env=USNM_SITEMAP_BUILD_MICROS={micros}");
}
