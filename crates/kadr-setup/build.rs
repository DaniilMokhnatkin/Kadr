use std::{env, path::PathBuf, process::Command};

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifests = crate_dir.parent().unwrap().parent().unwrap().join("manifests");
    // The setup runs with the user's own rights and asks for administrator rights only to install.
    let name = "app.manifest";
    let manifest = manifests.join(name);

    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    // trustInfo берём из своего манифеста, линкер не должен добавлять второй.
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");

    let icons = manifests.parent().unwrap().join("assets/icons");
    for file in ["kadr.rc", "kadr.ico", "tray-dark.ico", "tray-light.ico"] {
        println!("cargo:rerun-if-changed={}", icons.join(file).display());
    }
    println!("cargo:rerun-if-env-changed=RC");
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("kadr-icons.res");
    let rc = resource_compiler();
    let output = Command::new(&rc)
        .current_dir(&icons)
        .args(["/nologo", "/c65001", "/fo"])
        .arg(&resource)
        .arg("kadr.rc")
        .output()
        .expect("не удалось запустить Windows SDK rc.exe");
    assert!(
        output.status.success(),
        "rc.exe: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("cargo:rustc-link-arg-bins={}", resource.display());
}

fn resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return PathBuf::from(path);
    }
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return PathBuf::from("rc.exe");
    }
    let base = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("не задан ProgramFiles(x86)"))
        .join("Windows Kits/10/bin");
    let mut versions: Vec<_> = std::fs::read_dir(base)
        .expect("нужен Windows SDK с rc.exe")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("x64/rc.exe").is_file())
        .collect();
    versions.sort_by_key(|p| {
        p.file_name().unwrap().to_string_lossy().split('.').map(|s| s.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
    });
    versions.pop().expect("Windows SDK rc.exe не найден; задайте RC").join("x64/rc.exe")
}
