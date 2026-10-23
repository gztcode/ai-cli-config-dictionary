//! schema 来源的契约测试。
//!
//! 九家工具的 schema 分两类来源：官方发布的（联网抓）和从官方文档派生的
//! （随仓库分发）。这里把两边的约定钉住 —— 任何一个工具「既没有 URL 又没有
//! 本地文件」都会让字典变空，这种退化必须在测试里被拦住。

use cfg_explorer::app::{cache_name, local_schema_names};
use cfg_explorer::registry::Registry;
use std::path::PathBuf;

fn registry() -> Registry {
    let text = include_str!("../registry.toml");
    Registry::parse(text).expect("注册表应当能解析")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 1. 每家工具都有 schema 来源
#[test]
fn 每家工具都有_schema_来源() {
    let reg = registry();
    assert!(
        reg.tools.len() >= 9,
        "注册表里的工具太少: {}",
        reg.tools.len()
    );
    let mut no_src = Vec::new();
    for t in &reg.tools {
        if !t.has_schema() {
            no_src.push(t.id.clone());
        }
    }
    assert!(
        no_src.is_empty(),
        "这些工具既没有 schema_url 也没有 schema_file，字典会是空的: {no_src:?}"
    );
}

/// 2. 本地 schema 文件都在 schemas/ 下且是合法 JSON
#[test]
fn 本地_schema_文件齐全且合法() {
    let reg = registry();
    let dir = repo_root().join("schemas");
    let mut checked = 0;
    for t in &reg.tools {
        if t.schema_file.trim().is_empty() {
            continue;
        }
        let p = dir.join(&t.schema_file);
        assert!(p.exists(), "{} 的本地 schema 不存在: {}", t.id, p.display());
        let txt = std::fs::read_to_string(&p).expect("读本地 schema 失败");
        let v: serde_json::Value = serde_json::from_str(&txt)
            .unwrap_or_else(|e| panic!("{} 的本地 schema 不是合法 JSON: {e}", t.id));
        assert_eq!(
            v.get("type").and_then(|x| x.as_str()),
            Some("object"),
            "{} 的本地 schema 根应当是 object",
            t.id
        );
        let props = v
            .get("properties")
            .and_then(|x| x.as_object())
            .unwrap_or_else(|| panic!("{} 的本地 schema 没有 properties", t.id));
        assert!(
            !props.is_empty(),
            "{} 的本地 schema 一个键都没有（派生失败？）",
            t.id
        );
        checked += 1;
    }
    assert!(checked >= 5, "本地 schema 的工具太少: {checked}");
}

/// 3. 内置的本地 schema 与 schemas/ 目录一致
#[test]
fn 内置本地_schema_与目录一致() {
    let reg = registry();
    for t in &reg.tools {
        if t.schema_file.trim().is_empty() {
            continue;
        }
        let embedded = local_schema_names()
            .into_iter()
            .any(|(n, _)| n == t.schema_file);
        assert!(
            embedded,
            "{} 的 schema_file={} 没有被编译进二进制",
            t.id, t.schema_file
        );
    }
}

/// 4. 缓存文件名带工具 id 前缀 —— 两家工具用同一个末段名也不会互相覆盖
#[test]
fn 缓存文件名不会互相覆盖() {
    let a = cache_name("tool-a", "https://example.com/x/config.json");
    let b = cache_name("tool-b", "https://example.com/y/config.json");
    assert_ne!(a, b, "不同工具的同名末段必须落到不同缓存文件");
    assert!(a.starts_with("tool-a-"), "缓存名应带工具 id: {a}");
    assert!(a.ends_with("config.json"), "缓存名应保留末段: {a}");
    // 同一个工具的同一个 URL 每次算出来都一样（否则永远缓存不命中）
    assert_eq!(a, cache_name("tool-a", "https://example.com/x/config.json"));
    // 查询串不影响命名
    assert_eq!(
        cache_name("tool-a", "https://example.com/x/config.json?v=2"),
        a
    );
}

/// 5. 9 家工具的 id 不重复
#[test]
fn 工具_id_不重复() {
    let reg = registry();
    let mut ids: Vec<&str> = reg.tools.iter().map(|t| t.id.as_str()).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "注册表里有重复的工具 id");
}

/// 6. 升级后桌面 App 必须刷新注册表
///
/// 注册表是**应用自带的数据**。以前只在文件不存在时播种，于是升级后桌面 App
/// 还在用旧注册表 —— 旧版没有 `schema_file` 字段，新接进来的五家工具全是空的，
/// Codex 的 schema 地址也还是被 Cloudflare 挡的那个。
#[test]
fn 启动时会刷新过期的注册表() {
    let dir = std::env::temp_dir().join("cfg-registry-refresh");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let paths = cfg_explorer::app::AppPaths {
        config_dir: dir.clone(),
        cache_dir: dir.join("cache"),
        registry_path: dir.join("registry.toml"),
    };
    // 先放一份「旧版本」的注册表：没有 schema_file、Codex 地址还是被挡的
    std::fs::write(
        &paths.registry_path,
        "[[tool]]\nid = \"codex\"\ndisplay_name = \"Codex CLI\"\nschema_origin = \"official\"\nschema_url = \"https://developers.openai.com/codex/config-schema.json\"\nconfig_format = \"toml\"\n",
    )
    .unwrap();

    paths.seed_defaults();

    let refreshed = std::fs::read_to_string(&paths.registry_path).unwrap();
    assert!(
        refreshed.contains("schema_file"),
        "注册表没被刷新（还是没有 schema_file 字段）"
    );
    assert!(
        refreshed.contains("raw.githubusercontent.com/openai/codex"),
        "注册表没被刷新（Codex 的 schema 地址还是旧的）"
    );
    // 旧版本要留备份，不能悄悄丢掉
    let backup = dir.join("registry.user.toml");
    assert!(backup.exists(), "被替换的旧注册表没有备份");
    assert!(std::fs::read_to_string(&backup)
        .unwrap()
        .contains("developers.openai.com"));
}
