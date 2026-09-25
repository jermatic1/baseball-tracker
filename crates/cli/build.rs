fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_OAK").is_none() {
        return;
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
}
