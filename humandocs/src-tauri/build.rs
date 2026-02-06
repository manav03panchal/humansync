fn main() {
    tauri_build::build();

    // On iOS, compile stub symbols for deprecated/missing SystemConfiguration
    // APIs that the system-configuration crate v0.6 references unconditionally.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "ios" {
        cc::Build::new()
            .file("ios_stubs.c")
            .compile("ios_stubs");

        // Link SystemConfiguration for the APIs that DO exist on iOS
        // (SCNetworkReachability, SCDynamicStore)
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    }
}
