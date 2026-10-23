//! 前端 ↔ Rust 的 JSON 契约测试。
//!
//! 前端（`src/ui/app.js`）按字段名取值，这里逐个锁定这些字段名。
//! 任何一边改了字段名而另一边没跟上，这个测试立刻红。

use cfg_explorer::commands::{self, EditPreview, EditValidation, OpenedFile, ToolUsage};
use cfg_explorer::edit::{EditOp, SaveResult};
use cfg_explorer::scan::{MatchBy, ScanOptions, ScanResult};
use serde_json::{json, Value};

fn keys(v: &Value) -> Vec<String> {
    v.as_object().expect("期望是对象").keys().cloned().collect()
}

/// 每个测试用独立的临时目录（测试是并行跑的，共用一个路径会互相删文件）
fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cfg-contract-{tag}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn assert_has(v: &Value, want: &[&str]) {
    let got = keys(v);
    for w in want {
        assert!(
            got.iter().any(|g| g == w),
            "缺少字段 `{w}`，实际字段: {got:?}"
        );
    }
}

// ── 前端读到的字段 ─────────────────────────────────────

#[test]
fn scan_result_字段名与前端一致() {
    let dir = tmp_dir("scan");
    std::fs::write(dir.join("settings.json"), "{\"a\": 1}\n").unwrap();
    let opts = ScanOptions {
        roots: vec![dir.display().to_string()],
        filename: "settings".into(),
        known_only: true,
        ..Default::default()
    };
    let r: ScanResult = commands::scan_files(opts).unwrap();
    let v = serde_json::to_value(&r).unwrap();
    // renderScan 用到：result.files / result.error / result.elapsed_ms / result.truncated / result.cancelled
    assert_has(
        &v,
        &[
            "files",
            "error",
            "elapsed_ms",
            "truncated",
            "cancelled",
            "scanned",
        ],
    );

    // FoundFile 字段：path / size / modified / tool / matched_by / format
    let f = &r.files[0];
    let fv = serde_json::to_value(f).unwrap();
    assert_has(
        &fv,
        &["path", "size", "modified", "tool", "matched_by", "format"],
    );
    // matched_by 的取值前端要能区分「文件名 / 内容」
    assert!(matches!(f.matched_by, MatchBy::Filename | MatchBy::Content));
    assert!(matches!(
        fv["matched_by"].as_str(),
        Some("filename") | Some("content")
    ));
}

#[test]
fn loaded_file_字段名与前端一致() {
    let dir = tmp_dir("load");
    let p = dir.join("settings.json");
    std::fs::write(&p, "{\n  \"model\": \"x\"\n}\n").unwrap();

    let lf: OpenedFile = commands::open_file(p.to_str().unwrap()).unwrap();
    let v = serde_json::to_value(&lf).unwrap();
    // 编辑器用到：f.value / f.path / f.format / f.fingerprint / f.secret_paths
    // 刻意**不含** text：原文可能带明文密钥，不该送进前端
    assert_has(
        &v,
        &[
            "path",
            "format",
            "value",
            "is_new",
            "fingerprint",
            "secret_paths",
        ],
    );
    assert!(
        !keys(&v).iter().any(|k| k == "text"),
        "不该把原始文本送进前端"
    );
    assert_eq!(v["value"]["model"], "x");
    assert_eq!(v["format"], "json");

    std::fs::remove_file(&p).ok();
}

#[test]
fn edit_preview_字段名与前端一致() {
    let dir = tmp_dir("preview");
    let p = dir.join("settings.json");
    std::fs::write(&p, "{\n  \"model\": \"x\"\n}\n").unwrap();

    let ops = vec![EditOp::Set {
        path: vec!["model".into()],
        value: json!("y"),
    }];
    let ep: EditPreview = commands::preview_edit(p.to_str().unwrap(), &ops).unwrap();
    let v = serde_json::to_value(&ep).unwrap();
    // 前端用到：res.new_text / res.diff
    assert_has(&v, &["new_text", "diff"]);
    assert!(ep.new_text.contains("\"y\""));
    assert!(!ep.diff.is_empty());

    std::fs::remove_file(&p).ok();
}

#[test]
fn edit_validation_字段名与前端一致() {
    let dir = tmp_dir("validate");
    let p = dir.join("settings.json");
    std::fs::write(&p, "{\n  \"model\": \"x\"\n}\n").unwrap();

    let reg = cfg_explorer::Registry::parse(
        r#"
[[tool]]
id = "demo"
display_name = "Demo"
schema_url = ""
docs_url = ""
config_format = "json"
global_config = []
project_config = []
"#,
    )
    .unwrap();
    let cache = std::env::temp_dir().join("cfg-contract-cache");
    let ev: EditValidation =
        commands::validate_edit(&reg, &cache, p.to_str().unwrap(), &[]).unwrap();
    let v = serde_json::to_value(&ev).unwrap();
    // 校验面板用到：v.tool / v.schema_available / v.issues / v.error_count / v.warn_count / v.parse_error
    assert_has(
        &v,
        &[
            "tool",
            "schema_available",
            "issues",
            "error_count",
            "warn_count",
            "parse_error",
        ],
    );
    // 无 schema 的工具：能校验格式，但没有 key 级问题
    assert!(!ev.schema_available);
    assert!(ev.parse_error.is_none());

    // Issue 字段：path / level / message
    std::fs::remove_file(&p).ok();
}

#[test]
fn save_result_字段名与前端一致() {
    let dir = tmp_dir("save");
    let p = dir.join("settings.json");
    std::fs::write(&p, "{\n  \"model\": \"x\"\n}\n").unwrap();

    let ops = vec![EditOp::Set {
        path: vec!["model".into()],
        value: json!("z"),
    }];
    let sr: SaveResult = commands::commit_edit(p.to_str().unwrap(), &ops, None).unwrap();
    let v = serde_json::to_value(&sr).unwrap();
    // 前端用到：res.backup
    assert_has(&v, &["path", "bytes", "backup"]);
    assert!(sr.backup.is_some());
    assert!(std::fs::read_to_string(&p).unwrap().contains("\"z\""));

    std::fs::remove_file(&p).ok();
}

// ── 前端发过来的载荷 ───────────────────────────────────

#[test]
fn 前端发来的编辑载荷能正确反序列化() {
    // 前端 pushOp 产生的正是这种结构
    let raw = json!([{ "op": "set", "path": ["model"], "value": "claude-opus-4" }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        EditOp::Set { path, value } => {
            assert_eq!(path, &vec!["model".to_string()]);
            assert_eq!(value, &json!("claude-opus-4"));
        }
        _ => panic!("应当解析成 Set"),
    }

    // 数组下标路径
    let raw = json!([{ "op": "set", "path": ["permissions", "allow", "0"], "value": "Bash(ls)" }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    match &ops[0] {
        EditOp::Set { path, .. } => assert_eq!(path, &vec!["permissions", "allow", "0"]),
        _ => panic!("应当解析成 Set"),
    }

    // 删除
    let raw = json!([{ "op": "remove", "path": ["permissions", "deny"] }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    match &ops[0] {
        EditOp::Remove { path } => assert_eq!(path, &vec!["permissions", "deny"]),
        _ => panic!("应当解析成 Remove"),
    }

    // 对象 / 数组值
    let raw = json!([{ "op": "set", "path": ["env"], "value": { "A": "1" } }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    assert!(matches!(&ops[0], EditOp::Set { .. }));

    // 数组插入
    let raw = json!([{ "op": "insert", "path": ["allow"], "index": 1, "value": "Bash(ls)" }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    match &ops[0] {
        EditOp::Insert { path, index, value } => {
            assert_eq!(path, &vec!["allow".to_string()]);
            assert_eq!(*index, 1);
            assert_eq!(value, &json!("Bash(ls)"));
        }
        _ => panic!("应当解析成 Insert"),
    }

    // 数组元素移动
    let raw = json!([{ "op": "move", "path": ["allow"], "from": 2, "to": 0 }]);
    let ops: Vec<EditOp> = serde_json::from_value(raw).unwrap();
    match &ops[0] {
        EditOp::Move { path, from, to } => {
            assert_eq!(path, &vec!["allow".to_string()]);
            assert_eq!(*from, 2);
            assert_eq!(*to, 0);
        }
        _ => panic!("应当解析成 Move"),
    }
}

#[test]
fn 本机配置汇总的字段名与前端一致() {
    let dir = tmp_dir("usage");
    let cfg = dir.join("settings.json");
    std::fs::write(&cfg, "{\n  \"model\": \"x\"\n}\n").unwrap();
    // 路径要塞进 TOML 字符串，Windows 的反斜杠必须转义，
    // 否则 `\U` 会被当成「8 位十六进制 unicode 转义」而解析失败。
    let cfg_toml = cfg.display().to_string().replace('\\', "\\\\");
    let reg = cfg_explorer::Registry::parse(&format!(
        r#"
[[tool]]
id = "demo"
display_name = "Demo"
schema_url = ""
docs_url = ""
config_format = "json"
global_config = ["{}"]
project_config = []
"#,
        cfg_toml
    ))
    .unwrap();
    let usage: Vec<ToolUsage> = commands::local_usage(&reg, None);
    let v = serde_json::to_value(&usage).unwrap();
    // 前端用到：tool_id / display_name / config_path / values / secret_paths / error
    assert!(v.is_array());
    let u = &usage[0];
    let uv = serde_json::to_value(u).unwrap();
    assert_has(
        &uv,
        &[
            "tool_id",
            "display_name",
            "config_path",
            "values",
            "secret_paths",
            "error",
        ],
    );
    assert_eq!(u.values.get("model"), Some(&json!("x")));

    std::fs::remove_file(&cfg).ok();
}

#[test]
fn 提交编辑的冲突错误是结构化的() {
    let dir = tmp_dir("conflict");
    let p = dir.join("settings.json");
    std::fs::write(&p, "{\n  \"model\": \"x\"\n}\n").unwrap();

    // 打开时拿到指纹
    let lf = commands::open_file(p.to_str().unwrap()).unwrap();

    // 假装文件被别人改过
    std::fs::write(&p, "{\n  \"model\": \"y\"\n}\n").unwrap();

    let ops = vec![EditOp::Set {
        path: vec!["model".into()],
        value: json!("z"),
    }];
    let err = commands::commit_edit(p.to_str().unwrap(), &ops, Some(&lf.fingerprint)).unwrap_err();
    let payload: Value = serde_json::from_str(&err).expect("冲突错误应是 JSON 字符串");
    assert_eq!(payload["kind"], "conflict");
    assert_eq!(payload["expected"], lf.fingerprint.as_str());
    // 冲突时绝不能落盘
    assert!(std::fs::read_to_string(&p).unwrap().contains("\"y\""));

    std::fs::remove_file(&p).ok();
}

#[test]
fn 扫描参数能从前端载荷反序列化() {
    // 前端 startScan 里的 opts
    let raw = json!({
        "roots": ["~"],
        "filename": "settings",
        "content": "",
        "known_only": true,
        "max_size": 2097152,
        "limit": 500,
        "max_depth": 10
    });
    let opts: ScanOptions = serde_json::from_value(raw).unwrap();
    assert_eq!(opts.roots, vec!["~"]);
    assert_eq!(opts.filename, "settings");
    assert!(opts.known_only);
    assert_eq!(opts.limit, 500);

    // 缺字段时用默认值（前端漏传也不该炸）
    let opts: ScanOptions = serde_json::from_value(json!({})).unwrap();
    assert!(opts.known_only);
    assert_eq!(opts.max_depth, 10);
}
