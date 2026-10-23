//! 批量编辑的契约与行为测试。
//!
//! 批量改多个文件要保证三件事：
//!   1. 逐文件算预览，格式（缩进/注释）不丢
//!   2. 一个文件出错不影响其它文件的预览
//!   3. 任何一个文件被外部改过 → 整批不写盘（不留半吊子状态）

use cfg_explorer::commands::{self, BatchTarget};
use cfg_explorer::edit::EditOp;
use serde_json::json;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cfg-batch-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn set_op(key: &str, value: serde_json::Value) -> EditOp {
    EditOp::Set {
        path: vec![key.to_string()],
        value,
    }
}

#[test]
fn 批量预览保留各文件原有格式() {
    let d = tmp_dir("preview");
    let a = d.join("a.json");
    let b = d.join("b.toml");
    std::fs::write(
        &a,
        "{\n  \"model\": \"old-a\",\n  \"env\": {\n    \"FOO\": \"1\"\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(&b, "model = \"old-b\"\n").unwrap();

    let reg = cfg_explorer::registry::Registry { tools: vec![] };
    let targets = vec![
        BatchTarget {
            path: a.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
        BatchTarget {
            path: b.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
    ];
    let pv = commands::preview_edit_batch(&reg, &d, &targets);
    assert_eq!(pv.ok_count, 2);
    assert_eq!(pv.fail_count, 0);
    // JSON 的缩进和嵌套结构保持原样
    let ja = pv.files[0].new_text.clone();
    assert!(ja.contains("  \"model\": \"new\""), "缩进丢了: {ja}");
    assert!(ja.contains("  \"env\": {"), "嵌套结构丢了: {ja}");
    // TOML 还是 TOML
    assert!(pv.files[1].new_text.contains("model = \"new\""));
}

#[test]
fn 单个文件出错不影响其它文件() {
    let d = tmp_dir("one-bad");
    let good = d.join("good.json");
    let bad = d.join("bad.json");
    std::fs::write(&good, "{\n  \"model\": \"old\"\n}\n").unwrap();
    std::fs::write(&bad, "{ 这不是 JSON\n").unwrap();

    let reg = cfg_explorer::registry::Registry { tools: vec![] };
    let targets = vec![
        BatchTarget {
            path: good.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
        BatchTarget {
            path: bad.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
    ];
    let pv = commands::preview_edit_batch(&reg, &d, &targets);
    assert_eq!(pv.ok_count, 1);
    assert_eq!(pv.fail_count, 1);
    assert!(!pv.files[1].ok);
    assert!(pv.files[1].error.is_some());
}

#[test]
fn 有文件被外部改过则整批不写盘() {
    let d = tmp_dir("conflict");
    let a = d.join("a.json");
    let b = d.join("b.json");
    let orig_a = "{\n  \"model\": \"old-a\"\n}\n";
    std::fs::write(&a, orig_a).unwrap();
    std::fs::write(&b, "{\n  \"model\": \"old-b\"\n}\n").unwrap();

    // a 的指纹是错的（假装打开之后被人改过）
    let targets = vec![
        BatchTarget {
            path: a.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: Some("deadbeef".to_string()),
        },
        BatchTarget {
            path: b.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
    ];
    let res = commands::commit_edit_batch(&targets);
    assert_eq!(res.ok_count, 0);
    assert_eq!(res.fail_count, 2);
    assert!(res.saved.is_empty());
    // 两个文件都得保持原样
    assert_eq!(std::fs::read_to_string(&a).unwrap(), orig_a);
    assert!(std::fs::read_to_string(&b).unwrap().contains("old-b"));
}

#[test]
fn 没有冲突时整批写盘() {
    let d = tmp_dir("commit");
    let a = d.join("a.json");
    let b = d.join("b.json");
    std::fs::write(&a, "{\n  \"model\": \"old-a\"\n}\n").unwrap();
    std::fs::write(&b, "{\n  \"model\": \"old-b\"\n}\n").unwrap();

    let targets = vec![
        BatchTarget {
            path: a.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
        BatchTarget {
            path: b.to_string_lossy().to_string(),
            ops: vec![set_op("model", json!("new"))],
            base_fingerprint: None,
        },
    ];
    let res = commands::commit_edit_batch(&targets);
    assert_eq!(res.ok_count, 2);
    assert_eq!(res.saved.len(), 2);
    assert!(std::fs::read_to_string(&a).unwrap().contains("\"new\""));
    assert!(std::fs::read_to_string(&b).unwrap().contains("\"new\""));
}

#[test]
fn 批量删除键() {
    let d = tmp_dir("remove");
    let a = d.join("a.json");
    std::fs::write(&a, "{\n  \"model\": \"x\",\n  \"extra\": 1\n}\n").unwrap();

    let reg = cfg_explorer::registry::Registry { tools: vec![] };
    let targets = vec![BatchTarget {
        path: a.to_string_lossy().to_string(),
        ops: vec![EditOp::Remove {
            path: vec!["extra".to_string()],
        }],
        base_fingerprint: None,
    }];
    let pv = commands::preview_edit_batch(&reg, &d, &targets);
    assert_eq!(pv.ok_count, 1);
    assert!(!pv.files[0].new_text.contains("extra"));
    assert!(pv.files[0].new_text.contains("\"model\""));
}
