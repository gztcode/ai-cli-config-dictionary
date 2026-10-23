//! AI CLI 配置字典 · 桌面应用（Tauri 2）。
//!
//! 这一层只做「IPC 适配」：所有实际行为都在 `cfg_explorer::commands` 里，
//! 桌面 App 和 CLI 的 HTTP 桥共用同一份实现。

mod tray;

use cfg_explorer::app::AppPaths;
use cfg_explorer::commands::{self, EditPreview, EditValidation, OpenedFile, ToolUsage};
use cfg_explorer::edit::{EditOp, SaveResult};
use cfg_explorer::preview::PreviewData;
use cfg_explorer::registry::Registry;
use cfg_explorer::scan::{ScanOptions, ScanResult};
use std::sync::Mutex;
use tauri::State;

pub struct AppState {
    paths: AppPaths,
    registry: Registry,
    registry_source: String,
    data: Mutex<Option<PreviewData>>,
}

/// 取当前数据（首次调用时构建）
#[tauri::command]
async fn get_data(state: State<'_, AppState>) -> Result<PreviewData, String> {
    // 注意：锁的持有期不能跨 .await，否则 future 失去 Send
    let cached = state.data.lock().ok().and_then(|g| g.clone());
    if let Some(d) = cached {
        return Ok(d);
    }
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    let built = tauri::async_runtime::spawn_blocking(move || {
        // 首次运行时缓存是空的，先把 schema 拉下来
        let _ = cfg_explorer::app::ensure_schemas(&registry, &paths.cache_dir);
        commands::get_data(&registry, &paths.cache_dir, &paths.i18n_dir())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Ok(mut g) = state.data.lock() {
        *g = Some(built.clone());
    }
    Ok(built)
}

/// 重新拉取各家 schema 并重建数据
#[tauri::command]
async fn refresh_schemas(state: State<'_, AppState>) -> Result<PreviewData, String> {
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    let built = tauri::async_runtime::spawn_blocking(move || {
        let mut results = Vec::new();
        for t in &registry.tools {
            // 内置本地 schema 不需要联网抓
            if !t.schema_file.trim().is_empty() {
                continue;
            }
            if !t.has_schema() {
                continue;
            }
            match cfg_explorer::app::fetch_schema(&t.id, &t.schema_url, &paths.cache_dir) {
                Ok((bytes, _)) => results.push(format!("{} ✓ {}B", t.id, bytes)),
                Err(e) => results.push(format!("{} ✕ {}", t.id, e)),
            }
        }
        let data = commands::get_data(&registry, &paths.cache_dir, &paths.i18n_dir());
        (results, data)
    })
    .await
    .map_err(|e| e.to_string())?;

    let (_logs, data) = built;
    if let Ok(mut g) = state.data.lock() {
        *g = Some(data.clone());
    }
    Ok(data)
}

/// 用系统默认浏览器打开官方文档链接
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("只允许打开 http/https 链接".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &url]);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("打开链接失败: {e}"))
}

/// 把一段文字复制到系统剪贴板
///
/// 降级到「搜文档」时要把搜索词给用户，网页里能用 navigator.clipboard，
/// 桌面 App 里走系统命令更稳。
#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    use std::io::Write;
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("pbcopy");
        c.stdin(std::process::Stdio::piped());
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        // wl-copy（Wayland）优先，回落到 xclip / xsel
        let mut c = std::process::Command::new("wl-copy");
        if which("wl-copy").is_none() {
            c = if which("xclip").is_some() {
                let mut x = std::process::Command::new("xclip");
                x.args(["-selection", "clipboard"]);
                x
            } else {
                let mut x = std::process::Command::new("xsel");
                x.args(["--clipboard", "--input"]);
                x
            };
        }
        c.stdin(std::process::Stdio::piped());
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("clip");
        c.stdin(std::process::Stdio::piped());
        c
    };
    let mut child = cmd.spawn().map_err(|e| format!("复制失败: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("复制失败: {e}"))?;
    }
    child.wait().map_err(|e| format!("复制失败: {e}"))?;
    Ok(())
}

/// 在 PATH 里找一个可执行文件（Linux 剪贴板工具要挑）
#[cfg(target_os = "linux")]
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 在文件管理器中显示配置文件
#[tauri::command]
fn reveal_path(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err("路径不存在".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(&p);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(p.parent().unwrap_or(&p));
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg("/select,").arg(&p);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("操作失败: {e}"))
}

/// 应用信息（注册表来源、目录等）
#[tauri::command]
fn app_info(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "registry_source": state.registry_source,
        "config_dir": state.paths.config_dir.display().to_string(),
        "cache_dir": state.paths.cache_dir.display().to_string(),
        "tools": state.registry.tools.iter().map(|t| t.display_name.clone()).collect::<Vec<_>>(),
    })
}

// ── 扫描 / 编辑 ──────────────────────────────────────────

/// 扫描磁盘上的 AI CLI 配置文件
#[tauri::command]
async fn scan_files(options: ScanOptions) -> Result<ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || commands::scan_files(options))
        .await
        .map_err(|e| e.to_string())?
}

/// 某种扫描范围对应的根目录
#[tauri::command]
fn scan_roots(scope: String) -> Vec<String> {
    commands::scan_roots(&scope)
}

/// 打开一个配置文件用于编辑（返回脱敏后的视图）
#[tauri::command]
async fn open_file(path: String) -> Result<OpenedFile, String> {
    tauri::async_runtime::spawn_blocking(move || commands::open_file(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// 取某个密钥字段的明文（用户显式点「显示」时才走这条命令）
#[tauri::command]
async fn reveal_secret(path: String, key_path: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || commands::reveal_secret(&path, &key_path))
        .await
        .map_err(|e| e.to_string())
        .map(|v| serde_json::json!({ "value": v }))
}

/// 新建一个配置文件
#[tauri::command]
async fn create_file(path: String, template: String) -> Result<SaveResult, String> {
    tauri::async_runtime::spawn_blocking(move || commands::create_file(&path, &template))
        .await
        .map_err(|e| e.to_string())?
}

/// 某个工具可以新建的候选配置路径
#[tauri::command]
fn new_file_candidates(state: State<'_, AppState>, tool_id: String) -> Vec<String> {
    commands::new_file_candidates(&state.registry, &tool_id)
}

/// 汇总各工具在本机的实际配置（脱敏后）
#[tauri::command]
fn local_usage(state: State<'_, AppState>, project_root: Option<String>) -> Vec<ToolUsage> {
    commands::local_usage(&state.registry, project_root.as_deref())
}

/// 预览编辑结果：返回写回后的文本 + 逐行差异
#[tauri::command]
async fn preview_edit(path: String, ops: Vec<EditOp>) -> Result<EditPreview, String> {
    tauri::async_runtime::spawn_blocking(move || commands::preview_edit(&path, &ops))
        .await
        .map_err(|e| e.to_string())?
}

/// 保存前校验：把编辑后的内容按 schema 跑一遍校验器
#[tauri::command]
async fn validate_edit(
    state: State<'_, AppState>,
    path: String,
    ops: Vec<EditOp>,
) -> Result<EditValidation, String> {
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    tauri::async_runtime::spawn_blocking(move || {
        commands::validate_edit(&registry, &paths.cache_dir, &path, &ops)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 提交编辑：落盘（先写同目录临时文件再 rename），并留一份备份
///
/// 带 `base_fingerprint` 时会做外部改动检测。
#[tauri::command]
async fn commit_edit(
    path: String,
    ops: Vec<EditOp>,
    base_fingerprint: Option<String>,
) -> Result<SaveResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        commands::commit_edit(&path, &ops, base_fingerprint.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 检查文件在打开之后有没有被改过
#[tauri::command]
async fn check_conflict(
    path: String,
    base_fingerprint: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(serde_json::json!({
            "conflict": match commands::check_conflict(&path, &base_fingerprint) {
                Some(err) => serde_json::to_value(&err).unwrap_or(serde_json::Value::Null),
                None => serde_json::Value::Null,
            }
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 配置片段库：读出所有片段（附带「键在 schema 里不存在」的提示）
#[tauri::command]
async fn list_snippets(state: State<'_, AppState>) -> Result<Vec<commands::SnippetView>, String> {
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    tauri::async_runtime::spawn_blocking(move || {
        commands::list_snippets(&registry, &paths.cache_dir, &paths.snippets_path())
    })
    .await
    .map_err(|e| e.to_string())
}

/// 配置片段库：保存
#[tauri::command]
fn save_snippets(
    state: State<'_, AppState>,
    snippets: Vec<cfg_explorer::snippets::Snippet>,
) -> Result<(), String> {
    commands::save_snippets(&state.paths.snippets_path(), &snippets)
}

/// 把一个片段施加到指定文件上（只预览，不写盘）
#[tauri::command]
async fn apply_snippet(
    state: State<'_, AppState>,
    path: String,
    snippet_id: String,
) -> Result<commands::EditPreview, String> {
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    tauri::async_runtime::spawn_blocking(move || {
        commands::apply_snippet(
            &registry,
            &paths.cache_dir,
            &paths.snippets_path(),
            &path,
            &snippet_id,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 批量预览：一次算出多个文件的写回结果与校验
#[tauri::command]
async fn preview_edit_batch(
    state: State<'_, AppState>,
    targets: Vec<commands::BatchTarget>,
) -> Result<commands::BatchPreview, String> {
    let paths = state.paths.clone();
    let registry = state.registry.clone();
    tauri::async_runtime::spawn_blocking(move || {
        commands::preview_edit_batch(&registry, &paths.cache_dir, &targets)
    })
    .await
    .map_err(|e| e.to_string())
}

/// 批量提交：一次写多个文件
#[tauri::command]
async fn commit_edit_batch(
    targets: Vec<commands::BatchTarget>,
) -> Result<commands::BatchCommit, String> {
    tauri::async_runtime::spawn_blocking(move || commands::commit_edit_batch(&targets))
        .await
        .map_err(|e| e.to_string())
}

pub fn run() {
    let paths = AppPaths::discover();
    paths.seed_defaults();
    let (registry, registry_source) = paths.find_registry();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _sc, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        tray::focus_search(app);
                    }
                })
                .build(),
        )
        .manage(AppState {
            paths,
            registry,
            registry_source,
            data: Mutex::new(None),
        })
        .setup(|app| {
            // 托盘常驻：菜单里能唤起窗口、去扫描、退出
            if let Err(e) = tray::build_tray(app.handle()) {
                eprintln!("托盘图标创建失败：{e}");
            }
            // 全局快捷键（注册失败只告警，不影响应用启动）
            tray::register_shortcuts(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关窗口 = 藏到托盘，不是退出
            tray::handle_window_event(window, event);
        })
        .invoke_handler(tauri::generate_handler![
            get_data,
            refresh_schemas,
            open_external,
            copy_text,
            reveal_path,
            app_info,
            scan_files,
            scan_roots,
            open_file,
            reveal_secret,
            create_file,
            new_file_candidates,
            local_usage,
            preview_edit,
            validate_edit,
            commit_edit,
            check_conflict,
            preview_edit_batch,
            commit_edit_batch,
            list_snippets,
            save_snippets,
            apply_snippet
        ])
        .run(tauri::generate_context!())
        .expect("启动 AI 配置字典失败");
}
