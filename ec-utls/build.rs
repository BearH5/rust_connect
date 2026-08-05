fn main() {
    let dir = std::path::PathBuf::from("../utls-bridge");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");

    // 按平台选择库文件扩展名，并拷到 deps/ 确保运行时能加载。
    let (lib_name, rerun) = if cfg!(target_os = "windows") {
        ("ec_utls_bridge.dll", "ec_utls_bridge.dll")
    } else if cfg!(target_os = "linux") {
        ("ec_utls_bridge.so", "ec_utls_bridge.so")
    } else if cfg!(target_os = "macos") {
        ("ec_utls_bridge.dylib", "ec_utls_bridge.dylib")
    } else {
        return; // 不支持的平台，跳过
    };

    println!("cargo:rerun-if-changed={}/{}", dir.display(), rerun);

    let src = dir.join(lib_name);
    if !src.exists() {
        return; // 库还没构建（可能是首次或只改 Rust 代码），跳过拷贝
    }
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            let _ = std::fs::copy(&src, deps_dir.join(lib_name));
            let _ = std::fs::copy(&src, debug_dir.join(lib_name));
        }
    }
}
