//! Embeds the icon (`res/app.rc` → `app.res`) with `rc.exe` from the Windows SDK. Without the
//! SDK the build still works, the exe just has no icon.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.ico");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found (Windows SDK) – building without icon");
        return;
    };
    let res = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("app.res");
    let ok = Command::new(&rc).args(["/nologo", "/fo"]).arg(&res).arg("res/app.rc").status().is_ok_and(|s| s.success());
    if ok {
        // link.exe takes compiled resources like object files.
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=rc.exe failed – building without icon");
    }
}

/// `RC` if set, else the newest SDK version under `Windows Kits\10\bin` for this machine.
fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = std::env::var_os("RC") {
        return Some(rc.into());
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let root = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from)?.join(r"Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("10.")))
        .collect();
    versions.sort_by_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        name.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
    });
    versions.into_iter().rev().map(|v| v.join(arch).join("rc.exe")).find(|rc| rc.exists())
}
