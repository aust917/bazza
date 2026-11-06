fn main() {
    // Expose the target triple so the dev build can find `binaries/ffmpeg-<triple>`.
    let triple = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=BAZZA_TARGET_TRIPLE={triple}");
    // Expose the build variant (cpu | cuda | metal) for future packaging integration.
    println!("cargo:rerun-if-env-changed=BAZZA_VARIANT");
    let variant = std::env::var("BAZZA_VARIANT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| {
        if triple.contains("apple") { "metal".into() } else { "cpu".into() }
    });
    println!("cargo:rustc-env=BAZZA_VARIANT={variant}");
    // tauri-winres embeds the application's resources only into binary targets.
    // The real windowless integration test also links TaskDialogIndirect, which
    // needs Common Controls v6 before process entry. Keep its manifest separate
    // from the normal app resources and from the lightweight unit-test harness.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rerun-if-changed=tests/startup_gate.manifest");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/startup_gate.manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
    }
    tauri_build::build()
}
