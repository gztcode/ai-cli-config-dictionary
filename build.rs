//! 构建脚本：把项目 `cache/` 里的 schema 打成「离线种子」清单。
//!
//! 桌面 App 首次启动时如果本地缓存是空的，优先用这些内置种子填充，
//! 这样即使没有网络也能立刻用；之后点「刷新 schema」再拉最新版本。
//!
//! 文件名与 URL 末段一致（`schema.rs` 里的命名规则），所以按文件名即可自动对应。

use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dest = Path::new(&out_dir).join("seeds.rs");

    let cache = Path::new("cache");
    let mut src = String::from(
        "/// 内置 schema 种子：(文件名, 内容)。文件名与 schema URL 末段一致。\n\
         pub static SEEDS: &[(&str, &[u8])] = &[\n",
    );

    if let Ok(entries) = std::fs::read_dir(cache) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(name) = p.file_name().and_then(|x| x.to_str()) else {
                continue;
            };
            // include_bytes! 的路径相对「包含该宏的文件」解析，
            // 而生成的文件在 OUT_DIR，所以这里必须写绝对路径。
            let abs = match std::fs::canonicalize(&p) {
                Ok(a) => a,
                Err(_) => continue,
            };
            println!("cargo:rerun-if-changed={}", abs.display());
            src.push_str(&format!(
                "    ({:?}, include_bytes!({:?})),\n",
                name,
                abs.display().to_string()
            ));
        }
    }
    src.push_str("];\n");

    std::fs::write(&dest, src).expect("写入 seeds.rs 失败");
    println!("cargo:rerun-if-changed=cache");
}
