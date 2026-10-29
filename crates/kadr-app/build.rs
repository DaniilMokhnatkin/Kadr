use std::{env, path::PathBuf};

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifests = crate_dir.parent().unwrap().parent().unwrap().join("manifests");
    let name = if env::var_os("CARGO_FEATURE_UIACCESS").is_some() {
        "app.uiaccess.manifest"
    } else {
        "app.manifest"
    };
    let manifest = manifests.join(name);

    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    // trustInfo берём из своего манифеста, линкер не должен добавлять второй.
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
}
