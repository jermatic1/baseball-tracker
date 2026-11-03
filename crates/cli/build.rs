fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The web app's build output is embedded from web/build. Make sure the
    // directory exists so the crate compiles before `task web` has run; the
    // server then answers with a hint instead of pages.
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/build");
    std::fs::create_dir_all(&web).expect("create web/build");
    println!("cargo:rerun-if-changed=../../web/build");
    if std::env::var_os("CARGO_FEATURE_OAK").is_none() {
        return;
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
}
