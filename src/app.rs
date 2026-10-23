//! 应用数据装配层：把「注册表 + schema 缓存 + 本机配置」组装成 GUI 可直接消费的数据。
//!
//! CLI、HTML 导出、桌面 App 共用这里，保证三边看到的数据完全一致。

use crate::ir::{Ir, IrKey, IrStats, SchemaSource};
use crate::preview::{PreviewData, ToolPayload};
use crate::registry::{now_iso, sha256_hex, Registry, Tool};
use crate::validate::FileReport;
use crate::{config, schema, validate};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 一份配置文件的读取结果。
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub path: String,
    pub value: Option<Value>,
    pub error: Option<String>,
}

/// 内置的「文档派生」schema。
///
/// 这些工具没有发布机器可读的 schema，但官方文档里有结构化的字段表
/// （类型 / 默认值 / 说明齐全），据此派生出一份随仓库分发，
/// 避免「编造键位」或「伪造文档锚点」。
const LOCAL_SCHEMAS: &[(&str, &str)] = &[
    ("pi.json", include_str!("../schemas/pi.json")),
    (
        "grok-build.json",
        include_str!("../schemas/grok-build.json"),
    ),
    ("openclaw.json", include_str!("../schemas/openclaw.json")),
    ("hermes.json", include_str!("../schemas/hermes.json")),
    (
        "minimax-code.json",
        include_str!("../schemas/minimax-code.json"),
    ),
];

fn local_schema(name: &str) -> Option<&'static str> {
    let n = name.trim();
    LOCAL_SCHEMAS.iter().find(|(f, _)| *f == n).map(|(_, t)| *t)
}

/// 内置本地 schema 的清单（文件名 + 内容），供测试与「刷新」界面展示
pub fn local_schema_names() -> Vec<(&'static str, &'static str)> {
    LOCAL_SCHEMAS.to_vec()
}

/// 缓存文件名：带工具 id 前缀，避免两家工具用同一个末段名时互相覆盖
/// （比如都叫 `config.json`）。
pub fn cache_name(tool_id: &str, url: &str) -> String {
    let base = url
        .rsplit('/')
        .next()
        .unwrap_or("schema.json")
        .split('?')
        .next()
        .unwrap_or("schema.json");
    format!("{tool_id}-{base}")
}

/// 载入 schema（读缓存）。返回 (schema, 来源元信息, 缓存天数)。
pub fn load_schema(t: &Tool, cache_dir: &Path) -> Option<(Value, SchemaSource, u64)> {
    // 1) 内置的文档派生 schema
    if !t.schema_file.trim().is_empty() {
        if let Some(text) = local_schema(&t.schema_file) {
            let bytes = text.as_bytes();
            let digest = sha256_hex(bytes);
            let value: Value = serde_json::from_slice(bytes).ok()?;
            let source = SchemaSource {
                url: format!("builtin:schemas/{}", t.schema_file),
                origin: t.schema_origin.clone(),
                sha256: digest,
                fetched_at: now_iso(),
                bytes: bytes.len(),
            };
            return Some((value, source, 0));
        }
    }
    if !t.has_schema() {
        return None;
    }
    let path = cache_dir.join(cache_name(&t.id, &t.schema_url));
    let bytes = std::fs::read(&path).ok()?;
    let digest = sha256_hex(&bytes);
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let age_days = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
        .map(|d| d.as_secs() / 86400)
        .unwrap_or(0);
    let source = SchemaSource {
        url: t.schema_url.clone(),
        origin: t.schema_origin.clone(),
        sha256: digest,
        fetched_at: now_iso(),
        bytes: bytes.len(),
    };
    Some((value, source, age_days))
}

/// 载入多语言覆盖层（`<i18n_dir>/<locale>.json`），键形如 `tool:path`。
pub fn load_translations(i18n_dir: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();

    // 1) 内置词条打底：保证 CLI / 预览 / 桌面三条路径都能拿到新译文
    if let Ok(m) = serde_json::from_str::<HashMap<String, String>>(AppPaths::embedded_i18n()) {
        for (k, v) in m {
            if k.starts_with("__") {
                continue;
            }
            map.insert(format!("zh-CN|{k}"), v);
        }
    }

    // 2) 磁盘上的覆盖层优先（用户可以自己改）
    let Ok(entries) = std::fs::read_dir(i18n_dir) else {
        return map;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let locale = p
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_string();
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Ok(m) = serde_json::from_str::<HashMap<String, String>>(&txt) {
                for (k, v) in m {
                    // `__` 开头是元信息（如 __meta__），不是词条
                    if k.starts_with("__") {
                        continue;
                    }
                    map.insert(format!("{locale}|{k}"), v);
                }
            }
        }
    }
    map
}

/// 把覆盖层里的翻译写进 IR 节点。
pub fn apply_translations(keys: &mut [IrKey], tool_id: &str, map: &HashMap<String, String>) {
    for k in keys.iter_mut() {
        let key = format!("{tool_id}:{}", k.path);
        for locale in ["zh-CN", "en"] {
            if let Some(txt) = map.get(&format!("{locale}|{key}")) {
                k.translations.insert(locale.to_string(), txt.clone());
            }
        }
        apply_translations(&mut k.children, tool_id, map);
    }
}

/// 读取某工具的全部配置文件（全局 + 项目）。
pub fn read_tool_configs(t: &Tool) -> Vec<LoadedConfig> {
    let mut out = Vec::new();
    for p in t.global_config.iter().chain(t.project_config.iter()) {
        let full = crate::registry::expand(p);
        let disp = full.display().to_string();
        if !full.exists() {
            out.push(LoadedConfig {
                path: disp,
                value: None,
                error: None,
            });
            continue;
        }
        match config::read(&full, &t.config_format) {
            Ok(v) => out.push(LoadedConfig {
                path: disp,
                value: Some(v),
                error: None,
            }),
            Err(e) => out.push(LoadedConfig {
                path: disp,
                value: None,
                error: Some(e.to_string()),
            }),
        }
    }
    out
}

/// 为单个工具构建 IR（schema → IR）。
/// 内置的「文档补充层」。
///
/// 官方 schema 常常只给结构不给文字说明（或者压根没有逐键锚点），
/// 这一层把官方文档里的说明和章节链接按键路径补上去。
const DOC_SUPPLEMENTS: &[(&str, &str)] = &[
    (
        "gemini-cli-doc.json",
        include_str!("../schemas/gemini-cli-doc.json"),
    ),
    (
        "opencode-doc.json",
        include_str!("../schemas/opencode-doc.json"),
    ),
    (
        "hermes-doc.json",
        include_str!("../schemas/hermes-doc.json"),
    ),
    (
        "openclaw-doc.json",
        include_str!("../schemas/openclaw-doc.json"),
    ),
];

fn doc_supplement(name: &str) -> Option<&'static str> {
    let n = name.trim();
    DOC_SUPPLEMENTS
        .iter()
        .find(|(f, _)| *f == n)
        .map(|(_, t)| *t)
}

/// 在补充层里给一个 IR 键路径找条目。
///
/// 补充层里的路径来自 JSON Schema（动态段写作 `*`），
/// 而 IR 里的路径写作 `[*]`，两边要对齐；
/// 还要支持「逐级去掉前缀」——`server.port` 找不到时，
/// 可以退到 `server` 那一节（那一节确实讲到了 server）。
fn lookup_supplement<'a>(
    supplement: &'a serde_json::Map<String, Value>,
    path: &str,
) -> Option<&'a Value> {
    let star_path = path.replace("[*]", "*");
    if let Some(v) = supplement.get(path) {
        return Some(v);
    }
    if let Some(v) = supplement.get(&star_path) {
        return Some(v);
    }
    // 逐级退前缀：`a.b.c` → `a.b` → `a`
    let parts: Vec<&str> = path.split('.').collect();
    for take in (1..parts.len()).rev() {
        let prefix = parts[..take].join(".");
        if let Some(v) = supplement.get(&prefix) {
            return Some(v);
        }
        let prefix_star = prefix.replace("[*]", "*");
        if let Some(v) = supplement.get(&prefix_star) {
            return Some(v);
        }
    }
    None
}

/// 把文档补充层写进 IR 节点。
///
/// 补充层格式：`{ "键路径": { "description": "...", "doc_url": "..." } }`
fn apply_doc_supplement(keys: &mut [IrKey], supplement: &serde_json::Map<String, Value>) {
    for k in keys.iter_mut() {
        if let Some(item) = lookup_supplement(supplement, &k.path) {
            if let Some(d) = item.get("description").and_then(|x| x.as_str()) {
                if !d.trim().is_empty() && k.description.is_none() {
                    k.description = Some(d.to_string());
                }
            }
            if let Some(u) = item.get("doc_url").and_then(|x| x.as_str()) {
                if !u.is_empty() && !k.doc_urls.iter().any(|x| x == u) {
                    k.doc_urls.push(u.to_string());
                }
            }
        }
        apply_doc_supplement(&mut k.children, supplement);
    }
}

pub fn build_ir(t: &Tool, cache_dir: &Path, translations: &HashMap<String, String>) -> Option<Ir> {
    let (schema_value, source, _age) = load_schema(t, cache_dir)?;
    let mut keys = schema::convert(&schema_value);
    apply_translations(&mut keys, &t.id, translations);
    // 文档补充层：官方文档里的说明与章节锚点
    if !t.doc_supplement.trim().is_empty() {
        if let Some(text) = doc_supplement(&t.doc_supplement) {
            if let Ok(sup) = serde_json::from_str::<serde_json::Map<String, Value>>(text) {
                apply_doc_supplement(&mut keys, &sup);
            }
        }
    }
    let stats = schema::stats(&keys);
    Some(Ir {
        tool: t.id.clone(),
        display_name: t.display_name.clone(),
        docs_url: t.docs_url.clone(),
        source,
        generated_at: now_iso(),
        keys,
        stats,
    })
}

/// 组装 GUI 需要的全部数据。
pub fn build_payload(reg: &Registry, cache_dir: &Path, i18n_dir: &Path) -> PreviewData {
    let translations = load_translations(i18n_dir);
    let mut tools = Vec::new();

    for t in &reg.tools {
        let ir = build_ir(t, cache_dir, &translations);
        let (keys, stats, source) = match &ir {
            Some(ir) => (ir.keys.clone(), ir.stats.clone(), Some(ir.source.clone())),
            None => (Vec::new(), IrStats::default(), None),
        };

        // 合并所有配置文件的值为一个对象（后者覆盖前者），供前端查当前值。
        // **必须脱敏**：这个对象会进静态 HTML 导出，明文密钥不能落到磁盘上。
        let mut merged = serde_json::Map::new();
        let mut secret_paths: Vec<String> = Vec::new();
        let mut reports = Vec::new();

        for lc in read_tool_configs(t) {
            if let Some(e) = &lc.error {
                reports.push(FileReport {
                    path: lc.path,
                    exists: true,
                    parsed: false,
                    error: Some(e.clone()),
                    issues: vec![],
                    key_count: 0,
                });
                continue;
            }
            let Some(v) = lc.value else {
                reports.push(FileReport {
                    path: lc.path,
                    exists: false,
                    parsed: false,
                    error: None,
                    issues: vec![],
                    key_count: 0,
                });
                continue;
            };
            if let Some(ir) = &ir {
                let mut r = validate::validate_config(&v, &ir.keys, &lc.path);
                r.path = lc.path.clone();
                reports.push(r);
            }
            if let Some(obj) = v.as_object() {
                for (k, val) in obj {
                    merged.insert(k.clone(), val.clone());
                }
            }
        }

        // 脱敏合并结果，并记录哪些路径是密钥
        let (safe_merged, merged_secrets) = crate::secret::redact(&Value::Object(merged), &[]);
        secret_paths.extend(merged_secrets);

        tools.push(ToolPayload {
            id: t.id.clone(),
            display_name: t.display_name.clone(),
            docs_url: t.docs_url.clone(),
            schema_available: t.has_schema(),
            schema_note: if t.has_schema() {
                String::new()
            } else {
                format!(
                    "{} 尚无公开的机器可读 schema，需手写或从文档补齐",
                    t.display_name
                )
            },
            source,
            stats,
            keys,
            reports,
            config: safe_merged,
            secret_paths,
        });
    }

    PreviewData {
        generated_at: now_iso(),
        tools,
    }
}

/// 桌面 App 的运行时目录约定：
/// - 配置与翻译：`~/.config/cfg-explorer/`
/// - schema 缓存：`~/.cache/cfg-explorer/`
/// - 注册表：优先用户目录，其次当前目录，最后用内置副本
#[derive(Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub registry_path: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cfg-explorer");
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cfg-explorer");
        let registry_path = config_dir.join("registry.toml");
        AppPaths {
            config_dir,
            cache_dir,
            registry_path,
        }
    }

    pub fn ensure_dirs(&self) {
        let _ = std::fs::create_dir_all(&self.config_dir);
        let _ = std::fs::create_dir_all(self.i18n_dir());
        let _ = std::fs::create_dir_all(&self.cache_dir);
    }

    pub fn i18n_dir(&self) -> PathBuf {
        self.config_dir.join("i18n")
    }

    /// 配置片段库文件
    pub fn snippets_path(&self) -> PathBuf {
        self.config_dir.join("snippets.json")
    }

    /// 首次运行时把内置的注册表与翻译播种到用户目录，之后用户可自由改写
    pub fn seed_defaults(&self) {
        self.ensure_dirs();

        // 注册表是**应用自带的数据**，不是用户偏好 —— 每次启动都用内置副本刷新。
        //
        // 以前只在文件不存在时播种，于是升级后桌面 App 还在用旧注册表：
        // 旧版没有 `schema_file` 字段，新接进来的五家工具全是空的，
        // Codex 的 schema 地址也还是被 Cloudflare 挡的那个。
        // 改过的旧文件先备份成 registry.user.toml，不丢用户的东西。
        let embedded = Self::embedded_registry();
        if let Ok(cur) = std::fs::read_to_string(&self.registry_path) {
            if cur != embedded {
                let backup = self.registry_path.with_extension("user.toml");
                if std::fs::write(&backup, cur.as_bytes()).is_ok() {
                    eprintln!("注册表已更新，旧版本备份到 {}", backup.display());
                }
            }
        }
        if std::fs::write(&self.registry_path, embedded.as_bytes()).is_err() {
            eprintln!("写入注册表失败: {}", self.registry_path.display());
        }

        let zh = self.i18n_dir().join("zh-CN.json");
        let embedded = Self::embedded_i18n();
        if !zh.exists() {
            let _ = std::fs::write(&zh, embedded);
        } else {
            // 已存在的覆盖层要**合并**内置的新词条：
            // 应用升级带来的新翻译必须能进到已装环境里，
            // 但用户自己改过的条目不能被覆盖。
            if let Ok(current) = std::fs::read_to_string(&zh) {
                if let (Ok(mut map), Ok(builtin)) = (
                    serde_json::from_str::<serde_json::Map<String, Value>>(&current),
                    serde_json::from_str::<serde_json::Map<String, Value>>(embedded),
                ) {
                    let mut changed = false;
                    for (k, v) in builtin {
                        if !map.contains_key(&k) {
                            map.insert(k, v);
                            changed = true;
                        }
                    }
                    if changed {
                        if let Ok(text) = serde_json::to_string_pretty(&map) {
                            let _ = std::fs::write(&zh, text);
                        }
                    }
                }
            }
        }
    }

    /// 内置注册表内容（编译期嵌入，保证 App 永远有可用数据）
    pub fn embedded_registry() -> &'static str {
        include_str!("../registry.toml")
    }

    /// 内置中文翻译
    pub fn embedded_i18n() -> &'static str {
        include_str!("i18n/zh-CN.json")
    }

    /// 找注册表：用户目录 → 当前工作目录 → 内置副本
    pub fn find_registry(&self) -> (Registry, String) {
        // 当前目录的注册表优先 —— 在项目里跑 CLI 时，用的是项目里那份最新的；
        // 用户目录那份是桌面 App 播种的副本。
        let candidates = [PathBuf::from("registry.toml"), self.registry_path.clone()];
        for c in candidates {
            if c.exists() {
                if let Ok(r) = Registry::load(&c) {
                    return (r, c.display().to_string());
                }
            }
        }
        let embedded =
            Registry::parse(Self::embedded_registry()).unwrap_or_else(|_| Registry::default());
        (embedded, "内置注册表".to_string())
    }
}

/// 内置 schema 种子（构建期从 `cache/` 打包进来）
mod seeds {
    include!(concat!(env!("OUT_DIR"), "/seeds.rs"));
}
pub use seeds::SEEDS;

/// 确保缓存里有各家的 schema。
///
/// 优先级：本地缓存 → 内置离线种子 → 联网拉取。
/// 这样首次启动无需等待网络也能立即可用。
pub fn ensure_schemas(reg: &Registry, cache_dir: &Path) -> Vec<String> {
    let mut logs = Vec::new();
    for t in &reg.tools {
        // 内置本地 schema 随程序分发，不需要缓存
        if !t.schema_file.trim().is_empty() {
            continue;
        }
        if !t.has_schema() {
            continue;
        }
        let name = cache_name(&t.id, &t.schema_url);
        let path = cache_dir.join(&name);
        if path.exists() {
            continue;
        }
        // 先用内置种子
        if let Some((_, bytes)) = SEEDS.iter().find(|(n, _)| *n == name) {
            if std::fs::write(&path, bytes).is_ok() {
                logs.push(format!("{} ⓘ 内置种子 {}B", t.id, bytes.len()));
                continue;
            }
        }
        // 没有种子就联网
        match fetch_schema(&t.id, &t.schema_url, cache_dir) {
            Ok((bytes, _)) => logs.push(format!("{} ✓ {}B", t.id, bytes)),
            Err(e) => logs.push(format!("{} ✕ {}", t.id, e)),
        }
    }
    logs
}

/// 把 schema 下载到缓存目录。返回 (字节数, sha256)。
pub fn fetch_schema(tool_id: &str, url: &str, cache_dir: &Path) -> anyhow::Result<(usize, String)> {
    let path = cache_dir.join(cache_name(tool_id, url));

    let http = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(90)))
            .build(),
    );

    let mut last_err = String::new();
    for attempt in 1..=3 {
        let result = http
            .get(url)
            .header("User-Agent", "cfg-explorer/0.1 (+schema fetcher)")
            .header("Accept", "application/json, text/plain, */*")
            .call();
        match result {
            Ok(mut resp) => {
                let body = resp.body_mut().read_to_string()?;
                let bytes = body.into_bytes();
                let digest = sha256_hex(&bytes);
                std::fs::write(&path, &bytes)?;
                return Ok((bytes.len(), digest));
            }
            Err(ureq::Error::StatusCode(code)) => return Err(anyhow::anyhow!("HTTP {code}")),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(800 * attempt));
                }
            }
        }
    }
    Err(anyhow::anyhow!("{last_err}（已重试 3 次）"))
}
