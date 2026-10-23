//! 极简 HTTP 桥：把前端的 `invoke` 协议暴露成 HTTP。
//!
//! 用途有两个：
//!   1. **端到端验证** —— 浏览器里跑真实前端 + 真实命令实现，
//!      不用每次都打包桌面 App 才能点一遍
//!   2. **脚本化** —— 任何 HTTP 客户端都能调同一套命令
//!
//! 命令实现来自 `crate::commands`，与桌面 App 的 Tauri IPC 完全同源。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::commands;
use crate::edit::EditOp;
use crate::registry::Registry;
use crate::scan::ScanOptions;

pub struct ServeOptions {
    pub addr: String,
    pub registry: Registry,
    pub cache_dir: PathBuf,
    pub i18n_dir: PathBuf,
    /// 配置片段库文件
    pub snippets_path: PathBuf,
}

/// 启动服务（阻塞）
pub fn serve(opts: ServeOptions) -> std::io::Result<()> {
    let listener = TcpListener::bind(&opts.addr)?;
    let local = listener.local_addr()?;
    println!("AI 配置字典 · HTTP 桥已启动");
    println!("  地址: http://{local}/");
    println!("  接口: POST /invoke  {{\"cmd\":\"scan_files\",\"args\":{{...}}}}");
    println!("  按 Ctrl+C 停止");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = handle(s, &opts) {
                    eprintln!("请求处理失败: {e}");
                }
            }
            Err(e) => eprintln!("连接失败: {e}"),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, opts: &ServeOptions) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    // 请求行
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    // 头部
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim();
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = t.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let (status, mime, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/") => (
            "200 OK",
            "text/html; charset=utf-8",
            index_html().into_bytes(),
        ),
        ("GET", "/app.css") => (
            "200 OK",
            "text/css; charset=utf-8",
            include_bytes!("ui/app.css").to_vec(),
        ),
        ("GET", "/app.js") => (
            "200 OK",
            "application/javascript; charset=utf-8",
            include_bytes!("ui/app.js").to_vec(),
        ),
        ("POST", "/invoke") => {
            let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let cmd = req.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            let args = req.get("args").cloned().unwrap_or(json!({}));
            match dispatch(opts, cmd, &args) {
                Ok(v) => (
                    "200 OK",
                    "application/json; charset=utf-8",
                    json!({"ok": true, "result": v}).to_string().into_bytes(),
                ),
                Err(e) => (
                    "200 OK",
                    "application/json; charset=utf-8",
                    json!({"ok": false, "error": e}).to_string().into_bytes(),
                ),
            }
        }
        ("GET", "/favicon.ico") => ("204 No Content", "image/x-icon", Vec::new()),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

/// 命令分发（与桌面 App 的 IPC 同一套实现）
fn dispatch(opts: &ServeOptions, cmd: &str, args: &Value) -> Result<Value, String> {
    let path_arg = || -> Result<&str, String> {
        args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "缺少参数 path".to_string())
    };
    let ops_arg = || -> Result<Vec<EditOp>, String> {
        let v = args.get("ops").cloned().unwrap_or(json!([]));
        serde_json::from_value(v).map_err(|e| e.to_string())
    };
    let batch_targets_arg = |a: &Value| -> Result<Vec<commands::BatchTarget>, String> {
        let v = a.get("targets").cloned().unwrap_or(json!([]));
        serde_json::from_value(v).map_err(|e| e.to_string())
    };
    let to_value = |v: serde_json::Value| Ok(v);

    match cmd {
        "get_data" => {
            let _ = crate::app::ensure_schemas(&opts.registry, &opts.cache_dir);
            let data = commands::get_data(&opts.registry, &opts.cache_dir, &opts.i18n_dir);
            to_value(serde_json::to_value(data).map_err(|e| e.to_string())?)
        }
        "refresh_schemas" => {
            for t in &opts.registry.tools {
                // 内置本地 schema 不需要联网抓
                if !t.schema_file.trim().is_empty() {
                    continue;
                }
                if t.has_schema() {
                    let _ = crate::app::fetch_schema(&t.id, &t.schema_url, &opts.cache_dir);
                }
            }
            let data = commands::get_data(&opts.registry, &opts.cache_dir, &opts.i18n_dir);
            to_value(serde_json::to_value(data).map_err(|e| e.to_string())?)
        }
        "scan_files" => {
            let options: ScanOptions =
                serde_json::from_value(args.get("options").cloned().unwrap_or(json!({})))
                    .map_err(|e| e.to_string())?;
            let r = commands::scan_files(options)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "scan_roots" => {
            let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("home");
            to_value(json!(commands::scan_roots(scope)))
        }
        "open_file" => {
            let r = commands::open_file(path_arg()?)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "reveal_secret" => {
            let key = args
                .get("key_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少参数 key_path".to_string())?;
            let r = commands::reveal_secret(path_arg()?, key)?;
            to_value(json!({ "value": r }))
        }
        "create_file" => {
            let template = args
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("empty");
            let r = commands::create_file(path_arg()?, template)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "new_file_candidates" => {
            let tool = args
                .get("tool_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少参数 tool_id".to_string())?;
            to_value(json!(commands::new_file_candidates(&opts.registry, tool)))
        }
        "local_usage" => {
            let project_root = args.get("project_root").and_then(|v| v.as_str());
            to_value(json!(commands::local_usage(&opts.registry, project_root)))
        }
        "preview_edit" => {
            let r = commands::preview_edit(path_arg()?, &ops_arg()?)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "validate_edit" => {
            let r =
                commands::validate_edit(&opts.registry, &opts.cache_dir, path_arg()?, &ops_arg()?)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "commit_edit" => {
            let fp = args.get("base_fingerprint").and_then(|v| v.as_str());
            let r = commands::commit_edit(path_arg()?, &ops_arg()?, fp)?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "check_conflict" => {
            let fp = args
                .get("base_fingerprint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match commands::check_conflict(path_arg()?, fp) {
                Some(err) => to_value(
                    json!({ "conflict": serde_json::to_value(&err).unwrap_or(Value::Null) }),
                ),
                None => to_value(json!({ "conflict": Value::Null })),
            }
        }
        "list_snippets" => {
            let r = commands::list_snippets(&opts.registry, &opts.cache_dir, &opts.snippets_path);
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "save_snippets" => {
            let v = args.get("snippets").cloned().unwrap_or(json!([]));
            let snippets: Vec<crate::snippets::Snippet> =
                serde_json::from_value(v).map_err(|e| e.to_string())?;
            commands::save_snippets(&opts.snippets_path, &snippets)?;
            to_value(json!(null))
        }
        "apply_snippet" => {
            let sid = args
                .get("snippet_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少参数 snippet_id".to_string())?;
            let r = commands::apply_snippet(
                &opts.registry,
                &opts.cache_dir,
                &opts.snippets_path,
                path_arg()?,
                sid,
            )?;
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "preview_edit_batch" => {
            let targets = batch_targets_arg(args)?;
            let r = commands::preview_edit_batch(&opts.registry, &opts.cache_dir, &targets);
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "commit_edit_batch" => {
            let targets = batch_targets_arg(args)?;
            let r = commands::commit_edit_batch(&targets);
            to_value(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "app_info" => to_value(json!({
            "registry_source": "http-bridge",
            "config_dir": opts.cache_dir.display().to_string(),
            "cache_dir": opts.cache_dir.display().to_string(),
            "tools": opts.registry.tools.iter().map(|t| t.display_name.clone()).collect::<Vec<_>>(),
        })),
        // 桌面 App 用系统默认浏览器打开外链；网页版直接让浏览器自己处理，
        // 这个命令只是为了不让前端的调用报错。
        "open_external" => to_value(json!(null)),
        other => Err(format!("未知命令: {other}")),
    }
}

/// 桌面 App 的页面外壳 + 一个走 HTTP 的 `invoke` 垫片。
/// 垫片与 Tauri 的 `window.__TAURI__.core.invoke` 形状完全一致，
/// 所以前端代码一行都不用改。
fn index_html() -> String {
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
window.__TAURI__ = {{
  core: {{
    invoke: async function (cmd, args) {{
      const r = await fetch('/invoke', {{
        method: 'POST',
        headers: {{ 'Content-Type': 'application/json' }},
        body: JSON.stringify({{ cmd: cmd, args: args || {{}} }})
      }});
      const j = await r.json();
      if (!j.ok) throw new Error(j.error);
      return j.result;
    }}
  }}
}};
// 标记这是 HTTP 垫片而不是真的桌面 App —— 外链交给浏览器自己打开
window.__TAURI_SHIM__ = true;
</script>
<script>
{js}
</script>
</body>
</html>"#,
        css = include_str!("ui/app.css"),
        js = include_str!("ui/app.js"),
    )
}
