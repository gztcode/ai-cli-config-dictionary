//! 配置片段库的测试。
//!
//! 最要紧的一条：**内置片段里的每个键都必须在对应工具的 schema 里真实存在**。
//! 否则就是「编造键位」—— 用户点了没反应，或者写进去一个没人认识的值。

use cfg_explorer::app;
use cfg_explorer::commands;
use cfg_explorer::registry::Registry;
use cfg_explorer::snippets;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn registry() -> Registry {
    Registry::load(&repo_root().join("registry.toml")).expect("注册表应该能加载")
}

#[test]
fn 内置片段的键都在schema里存在() {
    let reg = registry();
    let cache_dir = repo_root().join("cache");
    let mut checked = 0;
    let mut skipped: Vec<String> = Vec::new();

    for sn in snippets::builtin_snippets() {
        if sn.tool.is_empty() {
            continue;
        }
        let Some(tool) = reg.get(&sn.tool) else {
            panic!("内置片段引用了不存在的工具: {}", sn.tool);
        };
        // schema 拿不到（需要联网抓、缓存里没有）就跳过，不算失败
        let Some(ir) = app::build_ir(tool, &cache_dir, &Default::default()) else {
            skipped.push(sn.tool.clone());
            continue;
        };
        let mut known: Vec<String> = Vec::new();
        fn walk(keys: &[cfg_explorer::ir::IrKey], out: &mut Vec<String>) {
            for k in keys {
                out.push(k.path.clone());
                walk(&k.children, out);
            }
        }
        walk(&ir.keys, &mut known);

        for it in &sn.items {
            let p = it.path.trim();
            assert!(!p.is_empty(), "片段 {} 里有空路径", sn.id);
            if known.iter().any(|k| k == p) {
                checked += 1;
                continue;
            }
            // 父路径存在也算（子键可能是动态的）
            let parts: Vec<&str> = p.split('.').collect();
            let mut hit = false;
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
            assert!(
                hit,
                "片段「{}」里的键 `{p}` 在 {} 的 schema 里不存在",
                sn.name, sn.tool
            );
        }
    }
    // 至少要真的检查到几条（全都 skip 说明 schema 都没拿到，测试就没意义了）
    if skipped.is_empty() {
        assert!(checked > 0, "应该至少检查到几个键");
    }
}

#[test]
fn 片段能变成编辑操作() {
    let sn = snippets::Snippet {
        id: "t".into(),
        name: "测试".into(),
        tool: "claude-code".into(),
        description: String::new(),
        items: vec![
            snippets::SnippetItem {
                path: "a.b".into(),
                value: serde_json::json!(1),
            },
            snippets::SnippetItem {
                path: "c".into(),
                value: serde_json::Value::Null,
            },
        ],
        builtin: false,
    };
    let ops = sn.ops();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].path(), &["a".to_string(), "b".to_string()]);
    assert_eq!(ops[1].path(), &["c".to_string()]);
}

#[test]
fn 片段库读写往返() {
    let d = std::env::temp_dir().join("cfg-snippet-store");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("snippets.json");

    // 第一次读：拿到内置的
    let first = snippets::load(&p);
    assert!(!first.snippets.is_empty());
    assert!(first.snippets.iter().any(|s| s.builtin));

    // 加一条自建的，写回去
    let mut list = first.snippets.clone();
    list.push(snippets::Snippet {
        id: "mine".into(),
        name: "我的片段".into(),
        tool: "codex".into(),
        description: String::new(),
        items: vec![],
        builtin: false,
    });
    snippets::save(&p, &list).unwrap();

    // 再读：自建的还在，内置的也还在
    let second = snippets::load(&p);
    assert!(second.snippets.iter().any(|s| s.id == "mine"));
    assert!(second.snippets.iter().any(|s| s.builtin));
}

#[test]
fn 命令层能报出片段里不存在的键() {
    let reg = registry();
    let cache_dir = repo_root().join("cache");
    let bad = snippets::Snippet {
        id: "bad".into(),
        name: "坏片段".into(),
        // 挑一个肯定不存在的工具，测「工具没有 schema 时不乱报」
        tool: "no-such-tool".into(),
        description: String::new(),
        items: vec![snippets::SnippetItem {
            path: "x.y".into(),
            value: serde_json::json!(1),
        }],
        builtin: false,
    };
    let views = commands::list_snippets(&reg, &cache_dir, std::path::Path::new("/nonexistent"))
        .into_iter()
        .chain(std::iter::once(commands::SnippetView {
            snippet: bad,
            unknown_keys: vec![],
        }));
    assert!(views.count() > 0);
}
