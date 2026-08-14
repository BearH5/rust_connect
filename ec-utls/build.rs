fn main() {
    let dir = std::path::PathBuf::from("../utls-bridge");

    // 按平台选库文件名。
    // Windows: mingw 导库 libec_utls_bridge.a（已有 lib 前缀），直接 link-search utls-bridge。
    // Linux/macOS: Go c-shared 产出 ec_utls_bridge.so/.dylib（无 lib 前缀），
    //   cargo 的 dylib=ec_utls_bridge 找 libec_utls_bridge.so，需复制成带前缀的副本。
    //   副本放 OUT_DIR（而非污染源码目录），link-search 指向 OUT_DIR，
    //   避开同目录残留的 Windows libec_utls_bridge.a（Linux ld 不认 COFF）。
    let lib_file = if cfg!(target_os = "windows") {
        "ec_utls_bridge.dll"
    } else if cfg!(target_os = "linux") {
        "ec_utls_bridge.so"
    } else if cfg!(target_os = "macos") {
        "ec_utls_bridge.dylib"
    } else {
        return; // 不支持的平台，跳过
    };

    let src = dir.join(lib_file);
    if !src.exists() {
        return; // 库还没构建（可能是首次或只改 Rust 代码），跳过
    }

    // link-search 路径：Windows 用源码目录（.a 在那）；Linux/macOS 用 OUT_DIR（副本在那）。
    let search_dir = if cfg!(target_os = "windows") {
        dir.clone()
    } else {
        // Linux/macOS：复制成 libec_utls_bridge.so 放到 OUT_DIR。
        let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
        let prefixed = format!("lib{}", lib_file);
        let _ = std::fs::copy(&src, out_dir.join(&prefixed));
        out_dir
    };

    println!("cargo:rustc-link-search=native={}", search_dir.display());
    println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");
    println!("cargo:rerun-if-changed={}/{}", dir.display(), lib_file);

    // 拷到 deps/ 和 target 顶层确保运行时能加载（测试、示例、bin）。
    // Linux/macOS 的 Go c-shared 产出 ec_utls_bridge.so（无 lib 前缀、无 SONAME），
    // 但链接器记录的 NEEDED 是 libec_utls_bridge.so（带前缀的链接用副本名），
    // 所以运行目录必须同时有带 lib 前缀的副本，否则动态链接器找不到。
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            // 无前缀原名（供代码按路径 dlopen 等场景）
            let _ = std::fs::copy(&src, deps_dir.join(lib_file));
            let _ = std::fs::copy(&src, debug_dir.join(lib_file));
            // Linux/macOS 额外拷带 lib 前缀的副本（匹配 NEEDED 条目）
            if !cfg!(target_os = "windows") {
                let prefixed = format!("lib{}", lib_file);
                let _ = std::fs::copy(&src, deps_dir.join(&prefixed));
                let _ = std::fs::copy(&src, debug_dir.join(&prefixed));
            }
        }
    }
}
