use std::{env, path::PathBuf};

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifests = crate_dir.parent().unwrap().parent().unwrap().join("manifests");
    let name = if env::var_os("CARGO_FEATURE_UIACCESS").is_some() { "app.uiaccess.manifest" } else { "app.manifest" };
    let manifest = manifests.join(name);

    println!("cargo:rerun-if-changed={}", manifest.display());
    // Our manifest (with its trustInfo, incl. uiAccess) replaces rustc's default one; the linker
    // embeds it. MANIFESTUAC:NO keeps the linker from adding a second trustInfo of its own.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");

    let icons = manifests.parent().unwrap().join("assets/icons");
    for file in ["kadr.ico", "tray-dark.ico", "tray-light.ico"] {
        println!("cargo:rerun-if-changed={}", icons.join(file).display());
    }

    // Icons and version info as a proper resource. A blank binary — no company, product or version
    // in its details — is what an antivirus reputation system reads as something built to hide, so
    // a real program names itself. winresource adds no manifest of its own, so it does not clash
    // with the linker-embedded manifest above. The version comes from the crate, so it stays in
    // step on its own. Set the publisher to the name the code-signing certificate will carry.
    let publisher = "Daniil Mokhnatkin";
    let icon = |file: &str| icons.join(file).to_string_lossy().into_owned();
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id(&icon("kadr.ico"), "1");
    res.set_icon_with_id(&icon("tray-dark.ico"), "101");
    res.set_icon_with_id(&icon("tray-light.ico"), "102");
    res.set("CompanyName", publisher);
    res.set("FileDescription", "Kadr - снимок экрана");
    res.set("ProductName", "Kadr");
    res.set("LegalCopyright", &format!("© 2026 {publisher}"));
    res.set("OriginalFilename", "kadr.exe");
    // FileVersion and ProductVersion default from CARGO_PKG_VERSION.
    res.compile().expect("не удалось встроить значок и версию");
}
