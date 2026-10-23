//! 与前端通信的命令实现。
//!
//! 这一层刻意**不依赖 Tauri**：桌面 App 的 IPC 和 CLI 的 HTTP 桥调用的是同一套函数，
//! 保证「App 里点出来的行为」和「脚本里跑到的行为」完全一致，
//! 也让整套命令可以在浏览器里用真实数据做端到端验证。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::app;
use crate::edit::{self, EditOp, SaveResult};
use crate::registry::Registry;
use crate::scan::{self, ScanOptions, ScanResult};
use crate::secret;
use crate::validate::Issue;

/// 打开后的文件视图（**脱敏后**交给前端）。
///
/// 注意：这里刻意**不含**原始文本 `text`——原文里可能有明文密钥，
/// 没必要把它送进 WebView。前端只用得到结构化的 `value`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedFile {
    pub path: String,
    pub format: edit::ConfigFormat,
    /// 已脱敏的配置树
    pub value: Value,
    pub is_new: bool,
    /// 内容指纹（sha256 前 16 位），用于检测外部改动
    pub fingerprint: String,
    /// 密钥字段的路径（点分形式）
    pub secret_paths: Vec<String>,
}

/// 提交编辑时的错误载荷。
///
/// 序列化成 JSON 字符串回传给前端；前端用 `kind` 区分处理方式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitError {
    /// 文件在打开之后被别的程序改过
    Conflict {
        path: String,
        /// 打开时的指纹
        expected: String,
        /// 当前磁盘上的指纹
        actual: String,
    },
    /// 其他失败
    Failed { message: String },
}

impl CommitError {
    /// 序列化成给前端看的载荷
    pub fn payload(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|e| format!("{{\"kind\":\"failed\",\"message\":\"{e}\"}}"))
    }
}

/// 批量预览：逐个文件算出写回后的文本、差异与校验结果
///
/// 单个文件失败不影响其它文件 —— 一次批量改十几个项目，
/// 有一个格式不对不该让整批都看不了。
pub fn preview_edit_batch(
    registry: &Registry,
    cache_dir: &Path,
    targets: &[BatchTarget],
) -> BatchPreview {
    let mut files = Vec::new();
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;

    for t in targets {
        let p = PathBuf::from(&t.path);
        let tool = detect_tool(registry, &p);
        let mut res = BatchFileResult {
            path: t.path.clone(),
            ok: false,
            error: None,
            new_text: String::new(),
            diff: Vec::new(),
            tool,
            schema_available: false,
            issues: Vec::new(),
            error_count: 0,
            warn_count: 0,
            parse_error: None,
        };

        let file = match edit::load(&p) {
            Ok(f) => f,
            Err(e) => {
                res.error = Some(e.to_string());
                fail_count += 1;
                files.push(res);
                continue;
            }
        };
        let new_text = match edit::apply_edits(&file, &t.ops) {
            Ok(s) => s,
            Err(e) => {
                res.error = Some(e.to_string());
                fail_count += 1;
                files.push(res);
                continue;
            }
        };
        res.diff = edit::diff_summary(&file.text, &new_text);

        match edit::parse_value(&new_text, file.format) {
            Ok(value) => {
                if let Some(ir) = res
                    .tool
                    .as_ref()
                    .and_then(|name| registry.tools.iter().find(|t| &t.display_name == name))
                    .and_then(|t| app::build_ir(t, cache_dir, &Default::default()))
                {
                    res.schema_available = true;
                    let report = crate::validate::validate_config(&value, &ir.keys, &t.path);
                    res.issues = report.issues.clone();
                    res.error_count = report.error_count();
                    res.warn_count = report.warn_count();
                }
            }
            Err(e) => {
                res.parse_error = Some(e.to_string());
            }
        }

        res.new_text = new_text;
        res.ok = true;
        ok_count += 1;
        files.push(res);
    }

    BatchPreview {
        files,
        ok_count,
        fail_count,
    }
}

/// 批量提交：一次改多个文件
///
/// **先全量检查再落盘**：任何一个文件在打开之后被改过，整批都拒绝写入
/// （避免改了一半、另一半没改的半吊子状态）。写入阶段逐个文件记结果。
pub fn commit_edit_batch(targets: &[BatchTarget]) -> BatchCommit {
    // 1) 先检查所有文件的外部改动
    let mut conflicts: Vec<String> = Vec::new();
    for t in targets {
        if let Some(base) = t.base_fingerprint.as_deref() {
            if !base.is_empty() {
                if let Some(err) = check_conflict(&t.path, base) {
                    conflicts.push(err.payload());
                }
            }
        }
    }
    if !conflicts.is_empty() {
        let files = targets
            .iter()
            .map(|t| BatchFileResult {
                path: t.path.clone(),
                ok: false,
                error: Some("有文件在打开之后被改过，整批未写入".to_string()),
                new_text: String::new(),
                diff: Vec::new(),
                tool: None,
                schema_available: false,
                issues: Vec::new(),
                error_count: 0,
                warn_count: 0,
                parse_error: None,
            })
            .collect();
        return BatchCommit {
            fail_count: targets.len(),
            ok_count: 0,
            saved: Vec::new(),
            files,
        };
    }

    // 2) 逐个写盘
    let mut files = Vec::new();
    let mut saved = Vec::new();
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    for t in targets {
        let mut res = BatchFileResult {
            path: t.path.clone(),
            ok: false,
            error: None,
            new_text: String::new(),
            diff: Vec::new(),
            tool: None,
            schema_available: false,
            issues: Vec::new(),
            error_count: 0,
            warn_count: 0,
            parse_error: None,
        };
        match commit_edit(&t.path, &t.ops, None) {
            Ok(_) => {
                res.ok = true;
                res.new_text = String::new();
                saved.push(t.path.clone());
                ok_count += 1;
            }
            Err(e) => {
                res.error = Some(e);
                fail_count += 1;
            }
        }
        files.push(res);
    }
    BatchCommit {
        files,
        ok_count,
        fail_count,
        saved,
    }
}

/// 某个工具在本机上的实际配置（脱敏后）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsage {
    pub tool_id: String,
    pub display_name: String,
    /// 配置文件路径
    pub config_path: String,
    /// 扁平化后的键值（点分路径 -> 值）
    pub values: BTreeMap<String, Value>,
    /// 密钥路径
    pub secret_paths: Vec<String>,
    /// 解析失败时的错误
    pub error: Option<String>,
}

/// 编辑预览（只算不写）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPreview {
    /// 写回后的完整文本
    pub new_text: String,
    /// 逐行差异
    pub diff: Vec<String>,
}

/// 编辑校验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditValidation {
    /// 识别到的工具（display_name），没有则为空
    pub tool: Option<String>,
    /// 该工具是否有 schema（有才能做 key 级校验）
    pub schema_available: bool,
    /// 逐条问题
    pub issues: Vec<Issue>,
    pub error_count: usize,
    pub warn_count: usize,
    /// 解析错误（格式不合法时这里有值）
    pub parse_error: Option<String>,
}

/// 片段库条目（附带「这个键在 schema 里不存在」的提示）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetView {
    #[serde(flatten)]
    pub snippet: crate::snippets::Snippet,
    /// 片段里写了、但对应工具的 schema 里没有的键
    pub unknown_keys: Vec<String>,
}

/// 读片段库
pub fn list_snippets(
    registry: &Registry,
    cache_dir: &Path,
    snippets_path: &Path,
) -> Vec<SnippetView> {
    let store = crate::snippets::load(snippets_path);
    // 同一工具的多个片段共用一份 IR —— 逐个片段重建太浪费
    let mut ir_cache: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    let mut out = Vec::new();
    for sn in store.snippets {
        let unknown = if sn.tool.is_empty() {
            Vec::new()
        } else {
            let known = ir_cache
                .entry(sn.tool.clone())
                .or_insert_with(|| known_paths(registry, cache_dir, &sn.tool));
            unknown_keys(known, &sn)
        };
        out.push(SnippetView {
            snippet: sn,
            unknown_keys: unknown,
        });
    }
    out
}

/// 某个工具的 IR 里所有键路径
fn known_paths(registry: &Registry, cache_dir: &Path, tool_id: &str) -> Option<Vec<String>> {
    let tool = registry.tools.iter().find(|t| t.id == tool_id)?;
    let ir = app::build_ir(tool, cache_dir, &Default::default())?;
    let mut out = Vec::new();
    fn walk(keys: &[crate::ir::IrKey], out: &mut Vec<String>) {
        for k in keys {
            out.push(k.path.clone());
            walk(&k.children, out);
        }
    }
    walk(&ir.keys, &mut out);
    Some(out)
}

/// 片段里哪些键在对应工具的 schema 里找不到
fn unknown_keys(known: &Option<Vec<String>>, sn: &crate::snippets::Snippet) -> Vec<String> {
    let Some(known) = known else {
        return Vec::new();
    };
    let mut bad = Vec::new();
    for it in &sn.items {
        let p = it.path.trim();
        if p.is_empty() {
            continue;
        }
        if known.iter().any(|k| k == p) {
            continue;
        }
        // 逐级回退：父路径存在也算（子键可能是动态的）
        let mut hit = false;
        let parts: Vec<&str> = p.split('.').collect();
        for take in (1..parts.len()).rev() {
            let prefix = parts[..take].join(".");
            if known
                .iter()
                .any(|k| k == &prefix || k == &format!("{prefix}[*]"))
            {
                hit = true;
                break;
            }
        }
        if !hit {
            bad.push(p.to_string());
        }
    }
    bad
}

/// 写片段库
pub fn save_snippets(
    snippets_path: &Path,
    snippets: &[crate::snippets::Snippet],
) -> Result<(), String> {
    crate::snippets::save(snippets_path, snippets)
}

/// 把片段施加到一个配置文件上，返回写回后的文本与差异
pub fn apply_snippet(
    registry: &Registry,
    cache_dir: &Path,
    snippets_path: &Path,
    path: &str,
    snippet_id: &str,
) -> Result<EditPreview, String> {
    let store = crate::snippets::load(snippets_path);
    let sn = store
        .snippets
        .iter()
        .find(|s| s.id == snippet_id)
        .ok_or_else(|| format!("找不到片段: {snippet_id}"))?;
    let _ = (registry, cache_dir);
    preview_edit(path, &sn.ops())
}

/// 批量编辑的一个目标文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchTarget {
    pub path: String,
    /// 要施加在这个文件上的操作
    pub ops: Vec<EditOp>,
    /// 打开时的指纹（可选，传了就做外部改动检测）
    pub base_fingerprint: Option<String>,
}

/// 批量预览里的单个文件结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchFileResult {
    pub path: String,
    /// 这个文件能不能改（不能的话 error 里有原因）
    pub ok: bool,
    pub error: Option<String>,
    /// 写回后的完整文本
    pub new_text: String,
    /// 逐行差异
    pub diff: Vec<String>,
    /// 校验结果
    pub tool: Option<String>,
    pub schema_available: bool,
    pub issues: Vec<Issue>,
    pub error_count: usize,
    pub warn_count: usize,
    pub parse_error: Option<String>,
}

/// 批量预览的完整结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchPreview {
    pub files: Vec<BatchFileResult>,
    /// 有多少个文件改得动
    pub ok_count: usize,
    /// 有多少个文件改不了
    pub fail_count: usize,
}

/// 批量提交的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCommit {
    pub files: Vec<BatchFileResult>,
    pub ok_count: usize,
    pub fail_count: usize,
    /// 实际写盘成功的文件
    pub saved: Vec<String>,
}

/// 扫描磁盘上的 AI CLI 配置文件
pub fn scan_files(options: ScanOptions) -> Result<ScanResult, String> {
    Ok(scan::scan(&options, &scan::Cancel::new()))
}

/// 某种扫描范围对应的根目录
///
/// - `home` 用户主目录
/// - `projects` 常见的项目存放目录（Projects / code / work / …）
/// - `disk` 整个磁盘
pub fn scan_roots(scope: &str) -> Vec<String> {
    match scope {
        "home" => vec!["~".to_string()],
        "projects" => scan::common_project_roots(),
        "disk" => vec!["/".to_string()],
        _ => vec![],
    }
}

/// 打开一个配置文件用于编辑（返回脱敏后的视图）
pub fn open_file(path: &str) -> Result<OpenedFile, String> {
    let f = edit::load(&PathBuf::from(path)).map_err(|e| e.to_string())?;
    let (value, secret_paths) = secret::redact(&f.value, &[]);
    Ok(OpenedFile {
        path: f.path,
        format: f.format,
        value,
        is_new: f.is_new,
        fingerprint: f.fingerprint,
        secret_paths,
    })
}

/// 取某个密钥字段的明文（用户显式点「显示」时才走这条命令）
pub fn reveal_secret(path: &str, key_path: &str) -> Result<String, String> {
    let f = edit::load(&PathBuf::from(path)).map_err(|e| e.to_string())?;
    match secret::get_by_dotted(&f.value, key_path) {
        Some(Value::String(s)) => Ok(s),
        Some(other) => Ok(other.to_string()),
        None => Err(format!("路径不存在: {key_path}")),
    }
}

/// 新建一个配置文件。
///
/// `template` 决定初始内容：`empty` 是空对象/空表，其余按格式给最小可用结构。
pub fn create_file(path: &str, template: &str) -> Result<SaveResult, String> {
    let p = PathBuf::from(path);
    if p.exists() {
        return Err(format!("文件已存在: {path}"));
    }
    let fmt = edit::detect_format(&p, "");
    let text = match template {
        "empty" => match fmt {
            edit::ConfigFormat::Toml => String::new(),
            edit::ConfigFormat::Yaml => "{}\n".to_string(),
            _ => "{}\n".to_string(),
        },
        "list" => match fmt {
            edit::ConfigFormat::Toml => "[]\n".to_string(),
            edit::ConfigFormat::Yaml => "[]\n".to_string(),
            _ => "[]\n".to_string(),
        },
        _ => match fmt {
            edit::ConfigFormat::Toml => String::new(),
            edit::ConfigFormat::Yaml => "{}\n".to_string(),
            _ => "{}\n".to_string(),
        },
    };
    // 先落盘再校验格式，确保新建出来的是合法文件
    edit::save(&p, &text, false).map_err(|e| e.to_string())
}

/// 某个工具可以新建的候选配置路径（全局 + 项目级）
pub fn new_file_candidates(registry: &Registry, tool_id: &str) -> Vec<String> {
    let Some(t) = registry.tools.iter().find(|t| t.id == tool_id) else {
        return vec![];
    };
    let mut out: Vec<String> = t.global_config.to_vec();
    out.extend(t.project_config.iter().map(|p| format!("<项目根>/{p}")));
    out
}

/// 预览编辑结果：返回写回后的文本 + 逐行差异
pub fn preview_edit(path: &str, ops: &[EditOp]) -> Result<EditPreview, String> {
    let p = PathBuf::from(path);
    let file = edit::load(&p).map_err(|e| e.to_string())?;
    let new_text = edit::apply_edits(&file, ops).map_err(|e| e.to_string())?;
    let diff = edit::diff_summary(&file.text, &new_text);
    Ok(EditPreview { new_text, diff })
}

/// 保存前校验：把编辑后的内容按 schema 跑一遍校验器
pub fn validate_edit(
    registry: &Registry,
    cache_dir: &Path,
    path: &str,
    ops: &[EditOp],
) -> Result<EditValidation, String> {
    let p = PathBuf::from(path);
    let file = edit::load(&p).map_err(|e| e.to_string())?;
    let new_text = edit::apply_edits(&file, ops).map_err(|e| e.to_string())?;

    let value = match edit::parse_value(&new_text, file.format) {
        Ok(v) => v,
        Err(e) => {
            return Ok(EditValidation {
                tool: detect_tool(registry, &p),
                schema_available: false,
                issues: vec![],
                error_count: 0,
                warn_count: 0,
                parse_error: Some(e.to_string()),
            })
        }
    };

    let tool = detect_tool(registry, &p);
    let ir = tool
        .as_ref()
        .and_then(|name| registry.tools.iter().find(|t| &t.display_name == name))
        .and_then(|t| app::build_ir(t, cache_dir, &Default::default()));

    let (issues, error_count, warn_count) = match &ir {
        Some(ir) => {
            let report = crate::validate::validate_config(&value, &ir.keys, path);
            (
                report.issues.clone(),
                report.error_count(),
                report.warn_count(),
            )
        }
        None => (vec![], 0, 0),
    };

    Ok(EditValidation {
        tool,
        schema_available: ir.is_some(),
        issues,
        error_count,
        warn_count,
        parse_error: None,
    })
}

/// 提交编辑：落盘（先写同目录临时文件再 rename），并留一份备份
///
/// `base_fingerprint` 是打开文件时拿到的指纹。传了就做**外部改动检测**：
/// 磁盘上的内容与打开时不一致就拒绝写入，让用户决定放弃 / 覆盖 / 重新载入。
pub fn commit_edit(
    path: &str,
    ops: &[EditOp],
    base_fingerprint: Option<&str>,
) -> Result<SaveResult, String> {
    let p = PathBuf::from(path);
    let file = edit::load(&p).map_err(|e| e.to_string())?;

    // 外部改动检测
    if let Some(base) = base_fingerprint {
        if !base.is_empty() && file.fingerprint != base {
            let err = CommitError::Conflict {
                path: path.to_string(),
                expected: base.to_string(),
                actual: file.fingerprint,
            };
            return Err(err.payload());
        }
    }

    let new_text = edit::apply_edits(&file, ops).map_err(|e| e.to_string())?;
    // 写回前再解析一次，确保不会把坏文件写进磁盘
    edit::parse_value(&new_text, file.format).map_err(|e| e.to_string())?;
    edit::save(&p, &new_text, true).map_err(|e| e.to_string())
}

/// 检查文件在打开之后有没有被改过（前端可在保存前先问一次）
pub fn check_conflict(path: &str, base_fingerprint: &str) -> Option<CommitError> {
    if base_fingerprint.is_empty() {
        return None;
    }
    let p = PathBuf::from(path);
    let current = match edit::load(&p) {
        Ok(f) => f.fingerprint,
        Err(e) => {
            return Some(CommitError::Failed {
                message: e.to_string(),
            })
        }
    };
    if current == base_fingerprint {
        None
    } else {
        Some(CommitError::Conflict {
            path: path.to_string(),
            expected: base_fingerprint.to_string(),
            actual: current,
        })
    }
}

/// 把配置树扁平化成「点分路径 -> 值」
fn flatten(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                out.insert(prefix.to_string(), Value::Object(Default::default()));
                return;
            }
            for (k, v) in map {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&p, v, out);
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.insert(prefix.to_string(), Value::Array(vec![]));
                return;
            }
            for (i, v) in items.iter().enumerate() {
                flatten(&format!("{prefix}.{i}"), v, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.clone());
        }
    }
}

/// 汇总各工具在本机的实际配置（脱敏后）。
///
/// `project_root` 给了的话，项目级配置也一并纳入。
pub fn local_usage(registry: &Registry, project_root: Option<&str>) -> Vec<ToolUsage> {
    let mut out: Vec<ToolUsage> = Vec::new();
    for t in &registry.tools {
        let mut candidates: Vec<PathBuf> = t
            .global_config
            .iter()
            .map(|p| crate::registry::expand(p))
            .collect();
        if let Some(root) = project_root {
            for p in &t.project_config {
                candidates.push(PathBuf::from(root).join(p));
            }
        }
        // 全局配置与项目级配置可能都存在，取第一个真实存在的即可
        let Some(path) = candidates.into_iter().find(|p| p.exists()) else {
            continue;
        };
        let loaded = match edit::load(&path) {
            Ok(f) => f,
            Err(e) => {
                out.push(ToolUsage {
                    tool_id: t.id.clone(),
                    display_name: t.display_name.clone(),
                    config_path: path.display().to_string(),
                    values: BTreeMap::new(),
                    secret_paths: vec![],
                    error: Some(e.to_string()),
                });
                continue;
            }
        };
        let (redacted, secret_paths) = secret::redact(&loaded.value, &[]);
        let mut values = BTreeMap::new();
        flatten("", &redacted, &mut values);
        out.push(ToolUsage {
            tool_id: t.id.clone(),
            display_name: t.display_name.clone(),
            config_path: loaded.path,
            values,
            secret_paths,
            error: None,
        });
    }
    out
}

/// 判断某个文件属于哪家工具：
/// 先按注册表里登记的路径精确匹配，再退回路径特征推断
pub fn detect_tool(registry: &Registry, path: &Path) -> Option<String> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for t in &registry.tools {
        for p in t.global_config.iter().chain(t.project_config.iter()) {
            let full = crate::registry::expand(p);
            let full = std::fs::canonicalize(&full).unwrap_or(full);
            if full == target {
                return Some(t.display_name.clone());
            }
        }
    }
    scan::guess_tool(path)
}

/// 通用 payload（配置字典数据），前端首屏用
pub fn get_data(
    registry: &Registry,
    cache_dir: &Path,
    i18n_dir: &Path,
) -> crate::preview::PreviewData {
    app::build_payload(registry, cache_dir, i18n_dir)
}

/// 把 `serde_json::Value` 形式的编辑操作转成 `EditOp`
pub fn ops_from_value(v: &Value) -> Result<Vec<EditOp>, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}
