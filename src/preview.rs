//! 生成静态 HTML 预览（可离线单文件分发）。
//!
//! UI 层（CSS/JS）与桌面 App 共用 `src/ui/` 下的同一份文件，
//! 保证「导出的网页」和「桌面应用」渲染结果完全一致，不会出现两套 UI 漂移。

use crate::ir::Ir;
use crate::validate::FileReport;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, Clone)]
pub struct ToolPayload {
    pub id: String,
    pub display_name: String,
    pub docs_url: String,
    pub schema_available: bool,
    pub schema_note: String,
    pub source: Option<crate::ir::SchemaSource>,
    pub stats: crate::ir::IrStats,
    pub keys: Vec<crate::ir::IrKey>,
    pub reports: Vec<FileReport>,
    pub config: Value,
    /// 本机配置里属于密钥的路径（点分形式），值已在 `config` 里脱敏
    #[serde(default)]
    pub secret_paths: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PreviewData {
    pub generated_at: String,
    pub tools: Vec<ToolPayload>,
}

/// 桌面 App 的前端用同一份 payload 结构（通过 IPC 传递）
pub type AppPayload = PreviewData;

fn escape_script(json: &str) -> String {
    json.replace("</script", "<\\/script").replace("</", "<\\/")
}

pub fn render(data: &PreviewData) -> String {
    let payload = escape_script(&serde_json::to_string(data).unwrap_or_else(|_| "{}".into()));
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>AI CLI 配置字典</title>
<style>
{css}
</style>
</head>
<body>
<header>
  <h1>AI CLI 配置字典</h1>
  <span class="meta" id="gen"></span>
  <button id="lang">EN</button>
</header>
<div class="wrap">
  <aside id="side"></aside>
  <main id="main"></main>
</div>
<script id="payload" type="application/json">{payload}</script>
<script>
{js}
</script>
</body>
</html>"#,
        css = include_str!("ui/app.css"),
        js = include_str!("ui/app.js"),
        payload = payload,
    )
}

/// 桌面 App 的 HTML 外壳（数据由 IPC 提供，不内嵌 payload）
pub fn render_shell() -> String {
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>AI CLI 配置字典</title>
<style>
{css}
</style>
</head>
<body>
<header>
  <h1>AI CLI 配置字典</h1>
  <span class="meta" id="gen"></span>
  <button id="lang">EN</button>
</header>
<div class="wrap">
  <aside id="side"></aside>
  <main id="main"></main>
</div>
<script>
{js}
</script>
</body>
</html>"#,
        css = include_str!("ui/app.css"),
        js = include_str!("ui/app.js"),
    )
}

/// 供桌面 App 前端直接引用的静态资源
pub fn ui_css() -> &'static str {
    include_str!("ui/app.css")
}

pub fn ui_js() -> &'static str {
    include_str!("ui/app.js")
}

/// 重新导出为字符串（避免未使用告警，同时方便测试）
pub fn payload_to_string(data: &PreviewData) -> String {
    serde_json::to_string(data).unwrap_or_default()
}

#[allow(dead_code)]
fn _unused(_: &Ir) {}
