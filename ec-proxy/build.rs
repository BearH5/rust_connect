fn main() {
    // ec-proxy 间接依赖 ec-utls（经 ec-protocol），运行时需要 utls-bridge.dll。
    // 复用 ec-protocol/build.rs 的拷贝逻辑：从 utls-bridge 拷到 deps/。
    let src = std::path::PathBuf::from("../utls-bridge/ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            if src.exists() {
                let _ = std::fs::copy(&src, deps_dir.join("ec_utls_bridge.dll"));
                let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/ec_utls_bridge.dll");
}
