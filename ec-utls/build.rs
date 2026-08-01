fn main() {
    // 链接 utls-bridge 的导入库（mingw 格式 .a，所以本 crate 用 gnu target）。
    let dir = std::path::PathBuf::from("../utls-bridge");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");
    println!("cargo:rerun-if-changed={}/ec_utls_bridge.dll", dir.display());
    println!("cargo:rerun-if-changed={}/libec_utls_bridge.a", dir.display());

    // 把 dll 拷到 exe 所在目录，确保运行时（测试、示例、bin）能加载到。
    // 否则会出现 STATUS_ENTRYPOINT_NOT_FOUND（加载到旧 dll）或 STATUS_DLL_NOT_FOUND。
    // OUT_DIR 形如 target/<triple>/debug/build/<pkg>/out，
    // exe 在 target/<triple>/debug/deps/，故从 OUT_DIR 往上 3 级到 debug/，再进 deps/。
    let src = dir.join("ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            let _ = std::fs::copy(&src, deps_dir.join("ec_utls_bridge.dll"));
            // 某些运行（cargo run 的 bin）在 debug/ 下加载，也拷一份。
            let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
        }
    }
}
