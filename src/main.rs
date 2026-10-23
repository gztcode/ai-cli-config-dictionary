//! AI CLI 配置字典 · 命令行入口。
//!
//! 桌面 App 与 HTML 导出共用 `cfg_explorer` 库里的同一套数据装配逻辑。

use anyhow::{Context, Result};
use cfg_explorer::{app, ir::Ir, preview, registry::now_iso, Registry};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "cfg-explorer", about = "AI CLI 配置字典", version)]
struct Cli {
    /// 注册表路径
    #[arg(long, default_value = "registry.toml")]
    registry: PathBuf,
    /// 工作目录（缓存与产物）
    #[arg(long, default_value = ".")]
    workdir: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出本机各工具的配置现状
    Discover,
    /// 拉取 schema 到缓存
    Fetch,
    /// schema → IR
    Build,
    /// 用真实配置校验
    Check,
    /// 生成 HTML 预览
    Preview,
    /// 跑完整流水线
    All,
    /// 扫描磁盘上的 AI CLI 配置文件
    Scan {
        /// 扫描根目录，可重复。默认用户主目录
        #[arg(long = "root", short = 'r')]
        root: Vec<String>,
        /// 文件名关键字（子串匹配）
        #[arg(long)]
        filename: Option<String>,
        /// 文件内容关键字（子串匹配）
        #[arg(long)]
        content: Option<String>,
        /// 允许按内容特征命中（不限于已知文件名）
        #[arg(long)]
        content_scan: bool,
        /// 结果条数上限
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// 目录最大深度
        #[arg(long, default_value_t = 10)]
        depth: usize,
    },
    /// 编辑配置文件（保格式写回）
    Edit {
        /// 配置文件路径
        path: PathBuf,
        /// 设置 key=value，可重复。key 用 . 分隔，数字段视为数组下标
        #[arg(long = "set")]
        set: Vec<String>,
        /// 删除 key，可重复
        #[arg(long = "remove")]
        remove: Vec<String>,
        /// 只预览差异，不写盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 启动 HTTP 桥：真实前端 + 真实命令实现
    Serve {
        /// 监听地址
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let reg = Registry::load(&cli.registry)?;
    let cache_dir = cli.workdir.join("cache");
    let out_dir = cli.workdir.join("out");
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::create_dir_all(out_dir.join("ir"))?;
    std::fs::create_dir_all(out_dir.join("i18n"))?;

    match cli.command {
        Cmd::Discover => cmd_discover(&reg),
        Cmd::Fetch => cmd_fetch(&reg, &cache_dir),
        Cmd::Build => cmd_build(&reg, &cache_dir, &out_dir),
        Cmd::Check => cmd_check(&reg, &out_dir),
        Cmd::Preview => cmd_preview(&reg, &cache_dir, &out_dir),
        Cmd::Scan {
            root,
            filename,
            content,
            content_scan,
            limit,
            depth,
        } => cmd_scan(root, filename, content, content_scan, limit, depth),
        Cmd::Edit {
            path,
            set,
            remove,
            dry_run,
        } => cmd_edit(&path, &set, &remove, dry_run),
        Cmd::Serve { addr } => cmd_serve(
            &reg,
            &cache_dir,
            &out_dir.join("i18n"),
            &cfg_explorer::app::AppPaths::discover().snippets_path(),
            &addr,
        ),
        Cmd::All => {
            cmd_fetch(&reg, &cache_dir)?;
            cmd_build(&reg, &cache_dir, &out_dir)?;
            cmd_check(&reg, &out_dir)?;
            cmd_preview(&reg, &cache_dir, &out_dir)
        }
    }
}

// ---------------------------------------------------------------- discover

fn cmd_discover(reg: &Registry) -> Result<()> {
    println!(
        "{:<14} {:<26} {:<8} CONFIG FILES",
        "TOOL", "FORMAT", "STATE"
    );
    println!("{}", "-".repeat(100));
    for t in &reg.tools {
        let mut found = 0usize;
        let mut total = 0usize;
        let mut files = Vec::new();
        for p in t.global_config.iter().chain(t.project_config.iter()) {
            let full = cfg_explorer::registry::expand(p);
            let exists = full.exists();
            total += 1;
            if exists {
                found += 1;
            }
            files.push(format!("{}{}", p, if exists { " ✓" } else { "" }));
        }
        println!(
            "{:<14} {:<26} {}/{}  {}",
            t.id,
            t.config_format,
            found,
            total,
            files.join("  ")
        );
    }
    Ok(())
}

// -------------------------------------------------------------------- fetch

fn cmd_fetch(reg: &Registry, cache_dir: &Path) -> Result<()> {
    println!("{:<14} {:<10} DETAIL", "TOOL", "RESULT");
    println!("{}", "-".repeat(90));
    for t in &reg.tools {
        if !t.schema_file.trim().is_empty() {
            println!(
                "{:<14} {:<10} 内置本地 schema（{}）",
                t.id, "LOCAL", t.schema_file
            );
            continue;
        }
        if !t.has_schema() {
            println!("{:<14} {:<10} 注册表中无 schema_url", t.id, "SKIP");
            continue;
        }
        match app::fetch_schema(&t.id, &t.schema_url, cache_dir) {
            Ok((bytes, digest)) => println!(
                "{:<14} {:<10} {} bytes  sha256:{}",
                t.id,
                "OK",
                bytes,
                &digest[..16]
            ),
            Err(e) => println!("{:<14} {:<10} {}", t.id, "FAIL", e),
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- build

fn cmd_build(reg: &Registry, cache_dir: &Path, out_dir: &Path) -> Result<()> {
    let translations = app::load_translations(&out_dir.join("i18n"));
    println!(
        "{:<14} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "TOOL", "KEYS", "LEAF", "DOC", "L.DOC", "ENUM", "DEF", "TR"
    );
    println!("{}", "-".repeat(80));
    for t in &reg.tools {
        // 缓存过期提示
        if let Some((_, _, age)) = app::load_schema(t, cache_dir) {
            if age > 7 {
                println!(
                    "{:<14} {:>8}  ⚠ 缓存已 {} 天未更新，建议重新 fetch",
                    t.id, "", age
                );
            }
        }
        let Some(ir) = app::build_ir(t, cache_dir, &translations) else {
            println!("{:<14} {:>8}", t.id, "no schema");
            continue;
        };
        let stats = ir.stats.clone();
        let out = out_dir.join("ir").join(format!("{}.json", t.id));
        std::fs::write(&out, serde_json::to_string_pretty(&ir)?)?;
        let pct = |a: usize, b: usize| -> String {
            if b == 0 {
                "-".into()
            } else {
                format!("{a}/{b}")
            }
        };
        println!(
            "{:<14} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}  -> {}",
            t.id,
            stats.total_keys,
            stats.leaf_keys,
            stats.with_doc_url,
            pct(stats.leaf_with_doc_url, stats.leaf_total),
            stats.with_enum,
            stats.with_default,
            stats.translated,
            out.display()
        );
    }
    Ok(())
}

// -------------------------------------------------------------------- check

fn cmd_check(reg: &Registry, out_dir: &Path) -> Result<()> {
    let mut total_err = 0usize;
    let mut total_warn = 0usize;
    println!(
        "{:<14} {:<58} {:>6} {:>6}",
        "TOOL", "CONFIG FILE", "ERROR", "WARN"
    );
    println!("{}", "-".repeat(90));
    for t in &reg.tools {
        let ir_path = out_dir.join("ir").join(format!("{}.json", t.id));
        let ir: Option<Ir> = std::fs::read_to_string(&ir_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

        for lc in app::read_tool_configs(t) {
            if let Some(e) = lc.error {
                println!(
                    "{:<14} {:<58} {:>6} {:>6}  解析失败: {}",
                    t.id, lc.path, "-", "-", e
                );
                continue;
            }
            let Some(v) = lc.value else { continue };
            let Some(ir) = &ir else { continue };
            let report = cfg_explorer::validate::validate_config(&v, &ir.keys, &lc.path);
            total_err += report.error_count();
            total_warn += report.warn_count();
            println!(
                "{:<14} {:<58} {:>6} {:>6}",
                t.id,
                lc.path,
                report.error_count(),
                report.warn_count()
            );
            for is in &report.issues {
                let tag = if is.level == cfg_explorer::validate::Level::Error {
                    "✕"
                } else {
                    "⚠"
                };
                println!("      {} {} — {}", tag, is.path, is.message);
            }
        }
    }
    println!("\n合计: {} 个错误, {} 个警告", total_err, total_warn);
    Ok(())
}

// ------------------------------------------------------------------ preview

fn cmd_preview(reg: &Registry, cache_dir: &Path, out_dir: &Path) -> Result<()> {
    let data = app::build_payload(reg, cache_dir, &out_dir.join("i18n"));
    let html = preview::render(&data);
    let out = out_dir.join("index.html");
    std::fs::write(&out, &html).with_context(|| format!("写入失败: {}", out.display()))?;
    println!("预览已生成: {}  ({})", out.display(), now_iso());
    Ok(())
}

// -------------------------------------------------------------------- scan

#[allow(clippy::too_many_arguments)]
fn cmd_scan(
    roots: Vec<String>,
    filename: Option<String>,
    content: Option<String>,
    content_scan: bool,
    limit: usize,
    depth: usize,
) -> Result<()> {
    let opts = cfg_explorer::scan::ScanOptions {
        roots: if roots.is_empty() {
            vec!["~".to_string()]
        } else {
            roots
        },
        filename: filename.unwrap_or_default(),
        content: content.unwrap_or_default(),
        known_only: !content_scan,
        max_size: 2 * 1024 * 1024,
        limit,
        max_depth: depth,
        detect_projects: true,
        max_projects: 5000,
    };
    let res = cfg_explorer::scan::scan(&opts, &cfg_explorer::scan::Cancel::new());
    if let Some(e) = &res.error {
        eprintln!("扫描失败: {e}");
        return Ok(());
    }
    println!(
        "扫描 {} 个文件，命中 {} 个配置文件，发现 {} 个项目，用时 {} ms{}\n",
        res.scanned,
        res.files.len(),
        res.projects.len(),
        res.elapsed_ms,
        if res.truncated {
            "（文件结果已达上限，项目列表仍完整）"
        } else {
            ""
        }
    );

    if !res.projects.is_empty() {
        let with_cfg: Vec<_> = res.projects.iter().filter(|p| p.config_count > 0).collect();
        let empty = res.projects.len() - with_cfg.len();
        println!("── 含 AI CLI 配置的项目（{} 个）──", with_cfg.len());
        for p in &with_cfg {
            println!(
                "  {:<4} {:<28} {}",
                format!("{}个", p.config_count),
                p.name,
                p.root
            );
        }
        if empty > 0 {
            println!("  （另有 {empty} 个项目暂未发现 AI CLI 配置文件）");
        }
        println!();
    }

    println!(
        "{:<9} {:<16} {:<10} {:<9} PATH",
        "FORMAT", "TOOL", "SIZE", "MATCH"
    );
    println!("{}", "-".repeat(100));
    for f in &res.files {
        println!(
            "{:<9} {:<16} {:<10} {:<9} {}",
            f.format,
            f.tool.as_deref().unwrap_or("-"),
            format!("{} B", f.size),
            match f.matched_by {
                cfg_explorer::scan::MatchBy::Filename => "文件名",
                cfg_explorer::scan::MatchBy::Content => "内容",
            },
            f.path
        );
    }
    Ok(())
}

// ------------------------------------------------------------------- serve

fn cmd_serve(
    reg: &Registry,
    cache_dir: &Path,
    i18n_dir: &Path,
    snippets_path: &Path,
    addr: &str,
) -> Result<()> {
    let opts = cfg_explorer::serve::ServeOptions {
        addr: addr.to_string(),
        registry: reg.clone(),
        cache_dir: cache_dir.to_path_buf(),
        i18n_dir: i18n_dir.to_path_buf(),
        snippets_path: snippets_path.to_path_buf(),
    };
    cfg_explorer::serve::serve(opts)?;
    Ok(())
}

// -------------------------------------------------------------------- edit
/// 把命令行给的字面量解析成 JSON 值
fn parse_cli_value(s: &str) -> serde_json::Value {
    let t = s.trim();
    match t {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        "null" => return serde_json::Value::Null,
        _ => {}
    }
    if let Ok(n) = t.parse::<i64>() {
        return serde_json::Value::from(n);
    }
    if let Ok(f) = t.parse::<f64>() {
        return serde_json::Value::from(f);
    }
    if t.starts_with('"') || t.starts_with('[') || t.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            return v;
        }
    }
    serde_json::Value::String(s.to_string())
}

fn cmd_edit(path: &Path, sets: &[String], removes: &[String], dry_run: bool) -> Result<()> {
    use cfg_explorer::edit::{self, EditOp};

    let file = edit::load(path)?;
    let mut ops: Vec<EditOp> = Vec::new();
    for s in sets {
        let Some((k, v)) = s.split_once('=') else {
            eprintln!("忽略无效的 --set（应为 key=value）: {s}");
            continue;
        };
        ops.push(EditOp::Set {
            path: k.split('.').map(|x| x.to_string()).collect(),
            value: parse_cli_value(v),
        });
    }
    for r in removes {
        ops.push(EditOp::Remove {
            path: r.split('.').map(|x| x.to_string()).collect(),
        });
    }
    if ops.is_empty() {
        eprintln!("没有要做的修改（用 --set key=value / --remove key）");
        return Ok(());
    }

    let new_text = edit::apply_edits(&file, &ops)?;
    let diff = edit::diff_summary(&file.text, &new_text);
    if diff.is_empty() {
        println!("内容没有变化，未写入。");
        return Ok(());
    }

    println!("--- 差异 ---");
    for d in &diff {
        println!("{d}");
    }

    if dry_run {
        println!("\n(--dry-run，未写盘)");
        return Ok(());
    }

    let res = edit::save(path, &new_text, true)?;
    println!("\n已保存: {}", res.path);
    if let Some(b) = &res.backup {
        println!("备份:   {b}");
    }
    Ok(())
}
