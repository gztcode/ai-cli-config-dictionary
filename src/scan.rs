//! 磁盘扫描：在指定目录里找出所有 AI CLI 的配置文件。
//!
//! 两条命中路径：
//!   1. **文件名匹配** —— 已知的高置信度配置文件名（settings.json / config.toml / opencode.json …）
//!   2. **内容特征匹配** —— 文件名不认识，但内容里出现 `ANTHROPIC_BASE_URL`、`mcpServers`
//!      这类只有 AI CLI 配置才会有的特征串
//!
//! 用自写的多线程遍历器（共享工作队列），并剪掉 node_modules / .git / target
//! 这类巨型目录 —— 否则全盘扫描会被几万个无关文件拖死。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 明确属于 AI CLI 的文件名（放在任何位置都算命中）
pub const KNOWN_NAMES: &[&str] = &[
    "opencode.json",
    "opencode.jsonc",
    "openclaw.json",
    "openclaw.yaml",
    "openclaw.yml",
    "models.json",
    "mcp.json",
    ".mcp.json",
    ".claude.json",
    "claude_desktop_config.json",
    "hermes.toml",
];

/// 通用文件名（`settings.json` / `config.yaml` …）。
///
/// 这类名字满地都是，只在 **AI CLI 的配置目录里** 才算命中 ——
/// 否则 `miniprogram/pages/settings/settings.json` 这种业务文件会被误当成配置。
pub const GENERIC_NAMES: &[&str] = &[
    "settings.json",
    "settings.local.json",
    "config.json",
    "config.toml",
    "config.yaml",
    "config.yml",
    "providers.json",
    "agents.json",
    "gemini.json",
    ".gemini.json",
];

/// 内容特征：出现即认为很可能是 AI CLI 的配置
pub const CONTENT_NEEDLES: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_",
    "model_providers",
    "model_catalog_json",
    "apiKeyHelper",
    "mcpServers",
    "GEMINI_API_KEY",
    "OPENAI_BASE_URL",
    "defaultModel",
    "custom_provider",
    "custom_providers",
];

/// 一律跳过的目录名（遍历时剪枝）
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    "Caches",
    "Application Support",
    ".Trash",
    "DerivedData",
    ".gradle",
    ".m2",
    "Pods",
    ".next",
    ".nuxt",
    "coverage",
    ".cache",
    ".pnpm",
];

/// 项目标志文件：目录里有这些之一，就认为它是一个「项目根」。
///
/// 强标志（版本库 / 包清单）几乎不会有误判；
/// 弱标志（`.claude` 之类）只在这个目录不是全局配置目录时才认。
pub const PROJECT_MARKERS_STRONG: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "pubspec.yaml",
    "mix.exs",
    "CMakeLists.txt",
    "pnpm-workspace.yaml",
    "setup.py",
    "requirements.txt",
    "Makefile",
    "tsconfig.json",
];

/// 弱标志：只有 `.claude/` 没有别的东西的目录也常是项目，但放在
/// `~/.claude/plugins/...` 这类插件缓存里就会误判，所以要额外排除。
pub const PROJECT_MARKERS_WEAK: &[&str] = &[
    ".claude",
    ".codex",
    ".gemini",
    ".openclaw",
    ".hermes",
    ".pi",
    ".minimax",
    ".grok",
    ".mcp.json",
    "opencode.json",
    "opencode.jsonc",
];

/// 已知属于「AI CLI 全局配置」的目录名。
///
/// 注意不含 `.config` —— 那是所有工具共用的大杂烩目录，
/// 里面的 `gh/config.yml`、`zed/settings.json` 都不是 AI CLI 的配置。
const CONFIG_DIR_NAMES: &[&str] = &[
    ".claude",
    ".codex",
    ".gemini",
    ".openclaw",
    ".hermes",
    ".pi",
    ".minimax",
    ".grok",
    "opencode",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchBy {
    /// 文件名命中
    Filename,
    /// 内容特征命中
    Content,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundFile {
    pub path: String,
    pub size: u64,
    pub modified: Option<String>,
    /// 猜出来属于哪个工具（可能为空）
    pub tool: Option<String>,
    pub matched_by: MatchBy,
    pub format: String,
    /// 所属项目根目录（不在任何项目里则为空）
    pub project: Option<String>,
}

/// 发现的一个项目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectHit {
    /// 项目根目录
    pub root: String,
    /// 目录名
    pub name: String,
    /// 命中的标志文件
    pub markers: Vec<String>,
    /// 项目内找到的 AI CLI 配置文件数
    pub config_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanOptions {
    /// 扫描根目录
    pub roots: Vec<String>,
    /// 文件名过滤（子串，留空表示不按文件名过滤）
    pub filename: String,
    /// 内容过滤（子串，留空表示不过滤内容）
    pub content: String,
    /// 是否只匹配已知配置文件名
    pub known_only: bool,
    /// 单文件大小上限
    pub max_size: u64,
    /// 结果条数上限
    pub limit: usize,
    /// 目录最大深度
    pub max_depth: usize,
    /// 是否同时发现项目
    pub detect_projects: bool,
    /// 项目数上限
    pub max_projects: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            roots: vec![],
            filename: String::new(),
            content: String::new(),
            known_only: true,
            max_size: 2 * 1024 * 1024,
            limit: 500,
            max_depth: 10,
            detect_projects: true,
            max_projects: 2000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub files: Vec<FoundFile>,
    /// 发现的项目
    pub projects: Vec<ProjectHit>,
    pub scanned: u64,
    pub elapsed_ms: u128,
    /// 是否因为达到 limit 而提前截断
    pub truncated: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

/// 取消令牌
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancel {
    pub fn new() -> Self {
        Cancel(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 猜一个配置文件属于哪个工具。
///
/// 按「路径段精确匹配」来判断，而不是全路径子串匹配 ——
/// 否则 `<项目>/.claude/` 旁边的 `openspec/config.yaml` 也会被误判成 Claude Code。
pub fn guess_tool(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let file = comps.last().cloned().unwrap_or_default();

    // 1) 目录段精确命中
    for c in comps.iter().rev().skip(1) {
        match c.as_str() {
            ".claude" => return Some("Claude Code".into()),
            ".codex" => return Some("Codex CLI".into()),
            ".gemini" => return Some("Gemini CLI".into()),
            ".openclaw" => return Some("OpenClaw".into()),
            ".hermes" => return Some("Hermes Agent".into()),
            ".pi" => return Some("Pi".into()),
            ".minimax" => return Some("MiniMax Code".into()),
            ".grok" => return Some("Grok Build".into()),
            "opencode" => return Some("OpenCode".into()),
            _ => {}
        }
    }
    // 2) 文件名命中
    match file.as_str() {
        "opencode.json" | "opencode.jsonc" => return Some("OpenCode".into()),
        "openclaw.json" | "openclaw.yaml" | "openclaw.yml" => return Some("OpenClaw".into()),
        "models.json" => return Some("Pi".into()),
        "hermes.toml" => return Some("Hermes Agent".into()),
        _ => {}
    }
    // 3) 目录名包含（`claude-code` 这类）
    for c in comps.iter().rev().skip(1) {
        if c.contains("claude") {
            return Some("Claude Code".into());
        }
        if c.contains("codex") {
            return Some("Codex CLI".into());
        }
        if c.contains("gemini") {
            return Some("Gemini CLI".into());
        }
        if c.contains("opencode") {
            return Some("OpenCode".into());
        }
        if c.contains("openclaw") {
            return Some("OpenClaw".into());
        }
        if c.contains("hermes") {
            return Some("Hermes Agent".into());
        }
    }
    None
}

/// 按扩展名/内容猜配置格式
pub fn guess_format(path: &Path, head: &str) -> &'static str {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let lower = name.to_lowercase();
    if lower.ends_with(".toml") {
        return "toml";
    }
    if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        return "yaml";
    }
    if lower.ends_with(".jsonc") {
        return "jsonc";
    }
    if lower.ends_with(".json") {
        return if head.contains("//") || head.contains("/*") {
            "jsonc"
        } else {
            "json"
        };
    }
    let t = head.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        "json"
    } else if t.contains('=') && !t.contains(':') {
        "toml"
    } else {
        "yaml"
    }
}

/// 内容特征探测时最多读多少字节（够覆盖绝大多数配置文件的特征位置）
const NEEDLE_SCAN_LIMIT: usize = 256 * 1024;

/// 是否是「可能装着配置」的文件扩展名
fn is_config_ext(name: &str) -> bool {
    name.ends_with(".json")
        || name.ends_with(".jsonc")
        || name.ends_with(".toml")
        || name.ends_with(".yaml")
        || name.ends_with(".yml")
}

fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// 包管理器缓存目录：里面也有 package.json / go.mod，但不是项目
fn is_cache_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/go/pkg/mod/")
        || s.contains("/.pnpm-store/")
        || s.contains("/.cargo/registry/")
        || s.contains("/.npm/_cacache/")
}

fn read_head(path: &Path, n: usize) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = vec![0u8; n.min(1 << 20)];
    let Ok(k) = f.read(&mut buf) else {
        return String::new();
    };
    buf.truncate(k);
    String::from_utf8_lossy(&buf).to_string()
}

/// 遍历任务：目录 + 深度 + 所属项目 + 是否位于全局配置目录内
struct DirTask {
    path: PathBuf,
    depth: usize,
    /// 所属项目根（继承自上层）
    project: Option<PathBuf>,
    /// 是否在 `~/.claude` 这类全局配置目录内（这类目录内部不再算项目）
    in_config_dir: bool,
}

/// 执行扫描（多线程）
pub fn scan(opts: &ScanOptions, cancel: &Cancel) -> ScanResult {
    let t0 = Instant::now();
    let scanned = Arc::new(AtomicU64::new(0));
    let found: Arc<Mutex<Vec<FoundFile>>> = Arc::new(Mutex::new(Vec::new()));
    let projects: Arc<Mutex<Vec<ProjectHit>>> = Arc::new(Mutex::new(Vec::new()));
    let truncated = Arc::new(AtomicBool::new(false));

    let mut roots: Vec<PathBuf> = Vec::new();
    for r in &opts.roots {
        let p = if r == "~" {
            match dirs::home_dir() {
                Some(h) => h,
                None => continue,
            }
        } else if let Some(rest) = r.strip_prefix("~/") {
            match dirs::home_dir() {
                Some(h) => h.join(rest),
                None => continue,
            }
        } else {
            PathBuf::from(r)
        };
        if p.exists() {
            roots.push(p);
        }
    }
    if roots.is_empty() {
        return ScanResult {
            files: vec![],
            projects: vec![],
            scanned: 0,
            elapsed_ms: 0,
            truncated: false,
            cancelled: false,
            error: Some("指定的扫描路径不存在".into()),
        };
    }

    let name_filter = opts.filename.trim().to_lowercase();
    let content_filter = opts.content.trim().to_lowercase();
    let home = dirs::home_dir();

    let queue: Arc<Mutex<VecDeque<DirTask>>> = Arc::new(Mutex::new(
        roots
            .into_iter()
            .map(|p| {
                // 扫描根本身就是配置目录（直接扫 ~/.claude）时，里面的一切都是全局配置
                let in_config_dir = p
                    .file_name()
                    .map(|n| CONFIG_DIR_NAMES.contains(&n.to_string_lossy().as_ref()))
                    .unwrap_or(false);
                DirTask {
                    path: p,
                    depth: 0,
                    project: None,
                    in_config_dir,
                }
            })
            .collect(),
    ));
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(4);

    std::thread::scope(|scope| {
        for _ in 0..nthreads {
            let queue = Arc::clone(&queue);
            let found = Arc::clone(&found);
            let projects = Arc::clone(&projects);
            let scanned = Arc::clone(&scanned);
            let truncated = Arc::clone(&truncated);
            let cancel = cancel.clone();
            let opts = opts.clone();
            let name_filter = name_filter.clone();
            let content_filter = content_filter.clone();
            let home = home.clone();

            scope.spawn(move || {
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let next = { queue.lock().ok().and_then(|mut q| q.pop_front()) };
                    let Some(task) = next else { break };
                    let dir = task.path;
                    let depth = task.depth;

                    let Ok(rd) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    let entries: Vec<_> = rd.flatten().collect();

                    // ── 判断这个目录是不是「项目根」
                    let mut markers: Vec<String> = Vec::new();
                    if opts.detect_projects && !task.in_config_dir {
                        for e in &entries {
                            let name = e.file_name().to_string_lossy().to_string();
                            if PROJECT_MARKERS_STRONG.contains(&name.as_str()) {
                                markers.push(name);
                            }
                        }
                        if markers.is_empty() {
                            // 弱标志：只有当这个目录不是家目录时才算项目
                            let is_home = home.as_deref() == Some(dir.as_path());
                            if !is_home {
                                for e in &entries {
                                    let name = e.file_name().to_string_lossy().to_string();
                                    if PROJECT_MARKERS_WEAK.contains(&name.as_str()) {
                                        markers.push(name);
                                    }
                                }
                            }
                        }
                    }
                    let is_project = !markers.is_empty();
                    if is_project {
                        let name = dir
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| dir.display().to_string());
                        let mut ps = projects.lock().unwrap();
                        if ps.len() < opts.max_projects {
                            ps.push(ProjectHit {
                                root: dir.display().to_string(),
                                name,
                                markers: markers.clone(),
                                config_count: 0,
                            });
                        }
                    }
                    // 项目归属：自己是项目就用自己，否则继承上层的
                    let project_root: Option<PathBuf> = if is_project {
                        Some(dir.clone())
                    } else {
                        task.project.clone()
                    };

                    for entry in entries {
                        let path = entry.path();
                        let Ok(ft) = entry.file_type() else { continue };

                        if ft.is_dir() {
                            if depth >= opts.max_depth {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().to_string();
                            if is_skipped_dir(&name) || is_cache_path(&path) {
                                continue;
                            }
                            // macOS 的隐藏目录里常有大量无关内容，但 .claude/.codex 必须进去
                            if name.starts_with('.') && !is_wanted_hidden(&name) {
                                continue;
                            }
                            if let Ok(mut q) = queue.lock() {
                                // 进入全局配置目录时清空项目归属：
                                // `~/.claude/settings.json` 是用户级配置，不属于任何项目
                                let child_project = if CONFIG_DIR_NAMES.contains(&name.as_str()) {
                                    if is_project {
                                        Some(dir.clone())
                                    } else {
                                        None
                                    }
                                } else {
                                    project_root.clone()
                                };
                                q.push_back(DirTask {
                                    path,
                                    depth: depth + 1,
                                    project: child_project,
                                    in_config_dir: task.in_config_dir
                                        || CONFIG_DIR_NAMES.contains(&name.as_str()),
                                });
                            }
                            continue;
                        }

                        if !ft.is_file() {
                            continue;
                        }
                        scanned.fetch_add(1, Ordering::Relaxed);

                        let name = entry.file_name().to_string_lossy().to_string();
                        let name_lower = name.to_lowercase();
                        let name_hit = !name_filter.is_empty() && name_lower.contains(&name_filter);
                        let known_hit = KNOWN_NAMES.contains(&name_lower.as_str())
                            || (task.in_config_dir && GENERIC_NAMES.contains(&name_lower.as_str()));

                        // 三个入口：文件名关键字 / 已知文件名 / 内容特征
                        // 「只匹配已知文件名」打开时，第三条通道关闭。
                        let ext_candidate = is_config_ext(&name_lower);
                        if !name_hit && !known_hit && (opts.known_only || !ext_candidate) {
                            continue;
                        }

                        let Ok(md) = entry.metadata() else {
                            continue;
                        };
                        if md.len() > opts.max_size {
                            continue;
                        }

                        // 需要全文内容过滤时读满，否则只读前面一段找特征串
                        let need_full = !content_filter.is_empty();
                        let read_len = if need_full {
                            opts.max_size as usize
                        } else {
                            NEEDLE_SCAN_LIMIT.min(opts.max_size as usize)
                        };
                        let head = read_head(&path, read_len);

                        // 显式内容过滤
                        if need_full && !head.to_lowercase().contains(&content_filter) {
                            continue;
                        }

                        // 内容特征命中
                        let needle_hit = CONTENT_NEEDLES.iter().any(|n| head.contains(n));
                        if !name_hit && !known_hit && !needle_hit {
                            continue;
                        }

                        let matched_by = if name_hit || known_hit {
                            MatchBy::Filename
                        } else {
                            MatchBy::Content
                        };

                        let f = FoundFile {
                            path: path.display().to_string(),
                            size: md.len(),
                            modified: md.modified().ok().map(|t| {
                                let d = t
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                format_ts(d)
                            }),
                            tool: guess_tool(&path),
                            matched_by,
                            format: guess_format(&path, &head).to_string(),
                            project: project_root.as_ref().map(|p| p.display().to_string()),
                        };
                        let mut g = found.lock().unwrap();
                        // 结果达到上限后就不再收，但**继续遍历**——
                        // 否则项目发现会被一起截断，拿到的项目列表就不完整了
                        if g.len() < opts.limit {
                            g.push(f);
                        } else {
                            truncated.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });

    let mut files = found.lock().unwrap().clone();
    let mut projects = projects.lock().unwrap().clone();
    let was_truncated = truncated.load(Ordering::Relaxed);

    // 浅目录优先，同层按路径排序
    files.sort_by(|a, b| {
        a.path
            .matches('/')
            .count()
            .cmp(&b.path.matches('/').count())
            .then_with(|| a.path.cmp(&b.path))
    });

    // 统计每个项目里的配置文件数
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in &files {
        if let Some(p) = &f.project {
            *counts.entry(p.clone()).or_insert(0) += 1;
        }
    }
    for p in &mut projects {
        p.config_count = counts.get(&p.root).copied().unwrap_or(0);
    }
    projects.sort_by(|a, b| {
        b.config_count
            .cmp(&a.config_count)
            .then_with(|| a.root.cmp(&b.root))
    });

    ScanResult {
        files,
        projects,
        scanned: scanned.load(Ordering::Relaxed),
        elapsed_ms: t0.elapsed().as_millis(),
        truncated: was_truncated,
        cancelled: cancel.is_cancelled(),
        error: None,
    }
}

/// 常见的项目存放目录（用于「扫描所有项目」的快捷入口）
pub fn common_project_roots() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for name in [
            "Projects",
            "projects",
            "Developer",
            "developer",
            "code",
            "Code",
            "work",
            "Work",
            "workspace",
            "Workspace",
            "repos",
            "src",
            "git",
            "Documents",
            "Desktop",
        ] {
            let p = home.join(name);
            if p.is_dir() {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

/// 值得进入的隐藏目录
fn is_wanted_hidden(name: &str) -> bool {
    matches!(
        name,
        ".claude"
            | ".codex"
            | ".gemini"
            | ".config"
            | ".openclaw"
            | ".hermes"
            | ".pi"
            | ".minimax"
            | ".grok"
            | ".cursor"
            | ".continue"
            | ".aider"
    )
}

/// 极简时间格式化（UTC）
fn format_ts(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, _) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// 常见扫描根目录
pub fn default_roots() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(h) = dirs::home_dir() {
        v.push(h.display().to_string());
    }
    v
}
