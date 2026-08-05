fn main() {
    tauri_build::build();

    // DLL/SO 部署：ec-app 间接依赖 utls-bridge，需拷到 exe 同目录。
    let lib_name = if cfg!(target_os = "windows") {
        "ec_utls_bridge.dll"
    } else if cfg!(target_os = "linux") {
        "ec_utls_bridge.so"
    } else if cfg!(target_os = "macos") {
        "ec_utls_bridge.dylib"
    } else {
        return;
    };

    let src = std::path::PathBuf::from(format!("../utls-bridge/{lib_name}"));
    if src.exists() {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let out_path = std::path::PathBuf::from(&out_dir);
            if let Some(debug_dir) = out_path.ancestors().nth(3) {
                let _ = std::fs::copy(&src, debug_dir.join(lib_name));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/{lib_name}");

    // 监控 ui/dist：dist 内容变化时强制 tauri_build 重新嵌入资源。
    println!("cargo:rerun-if-changed=../ui/dist/index.html");
    println!("cargo:rerun-if-changed=../ui/dist/assets");
}
