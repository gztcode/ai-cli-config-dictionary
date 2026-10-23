use std::path::Path;

fn main() {
    // UI 资源只有一份源（核心库的 src/ui/），构建时复制到前端目录，
    // 避免「导出的网页」和「桌面 App」出现两套漂移的界面。
    let ui_src = Path::new("../../src/ui");
    let ui_dst = Path::new("../ui");
    if let Err(e) = std::fs::create_dir_all(ui_dst) {
        println!("cargo:warning=创建前端目录失败: {e}");
    }
    for f in ["app.css", "app.js"] {
        let from = ui_src.join(f);
        let to = ui_dst.join(f);
        println!("cargo:rerun-if-changed={}", from.display());
        if from.exists() {
            if let Err(e) = std::fs::copy(&from, &to) {
                println!("cargo:warning=复制 {} 失败: {e}", f);
            }
        } else {
            println!("cargo:warning=找不到 UI 资源 {}", from.display());
        }
    }

    tauri_build::build();
}
