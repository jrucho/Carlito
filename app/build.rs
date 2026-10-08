fn main() {
    if std::env::var("CARGO_FEATURE_TAKEOVER").is_ok() {
        // libquill.so + libqsgepaper.so from the quill project.
        let quill = concat!(env!("CARGO_MANIFEST_DIR"), "/../quill");
        println!("cargo:rustc-link-search=native={quill}/build");
        println!("cargo:rustc-link-search=native={quill}/vendor");
        println!("cargo:rustc-link-lib=dylib=quill");
        println!("cargo:rustc-link-lib=dylib=qsgepaper");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/home/root/quill:/usr/lib/plugins/scenegraph");
        // Resolve libquill's transitive Qt deps at link time from the SDK
        // sysroot. rpath-link only (NOT link-search: the SDK's libc/libm are
        // linker scripts with absolute paths that break outside --sysroot).
        let sdk = std::env::var("RM_SDK")
            .or_else(|_| std::env::var("HOME").map(|home| format!("{home}/rm-sdk-3.26")));
        if let Ok(sdk) = sdk {
            let sysroot = format!("{sdk}/sysroots/cortexa53-crypto-remarkable-linux/usr/lib");
            if std::path::Path::new(&sysroot).exists() {
                println!("cargo:rustc-link-arg=-Wl,-rpath-link,{sysroot}");
            }
        }
    }
}
