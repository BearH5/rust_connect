fn main() {
    // ec-protocol 依赖 ec-utls（间接依赖 utls-bridge.dll）。
    // 确保测试/示例运行时能在 exe 同目录加载到 DLL。
    // 逻辑与 ec-utls/build.rs 一致：从 utls-bridge 拷贝到 OUT_DIR 往上 3 级的 deps/。
    let src = std::path::PathBuf::from("../utls-bridge/ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        // OUT_DIR 形如 target/<triple>/debug/build/<pkg>/out，
        // exe 在 target/<triple>/debug/deps/，往上 3 级到 debug/，再进 deps/。
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            if src.exists() {
                let _ = std::fs::copy(&src, deps_dir.join("ec_utls_bridge.dll"));
                // 某些运行（cargo run 的 bin）在 debug/ 下加载，也拷一份。
                let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/ec_utls_bridge.dll");
}
