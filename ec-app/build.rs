fn main() {
    tauri_build::build();
    // DLL 部署：ec-app 间接依赖 utls-bridge.dll
    let src = std::path::PathBuf::from("../utls-bridge/ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            if src.exists() {
                let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/ec_utls_bridge.dll");
}
