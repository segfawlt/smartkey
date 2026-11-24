use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=smartkey_tsf.def");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let def_path = manifest_dir.join("smartkey_tsf.def");
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def_path.display());
    }
}
