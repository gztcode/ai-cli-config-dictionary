//! 保格式编辑的回归测试。
//!
//! 核心命题：**只改用户动的那几个字节**，其余（缩进、注释、键顺序）原样保留。

use cfg_explorer::edit::{self, EditOp, LoadedFile};
use serde_json::{json, Value};

/// JSON 源文件带注释，serde_json 是严格解析器，这里用自写的 JSONC 解析器
fn parse_jsonc(text: &str) -> Value {
    cfg_explorer::jsonc::parse(text)
        .expect("JSONC 解析失败")
        .value
}

fn file(path: &str, text: &str) -> LoadedFile {
    LoadedFile {
        path: path.to_string(),
        format: edit::detect_format(std::path::Path::new(path), text),
        text: text.to_string(),
        value: Value::Null,
        is_new: false,
        fingerprint: edit::fingerprint(text),
    }
}

fn set(path: &[&str], value: Value) -> EditOp {
    EditOp::Set {
        path: path.iter().map(|s| s.to_string()).collect(),
        value,
    }
}

fn rm(path: &[&str]) -> EditOp {
    EditOp::Remove {
        path: path.iter().map(|s| s.to_string()).collect(),
    }
}

// ────────────────────────────────────────────── JSON

const JSON_SRC: &str = r#"{
  // 顶部注释
  "model": "claude-opus-4",
  "env": {
    "ANTHROPIC_BASE_URL": "https://api.example.com"
  },
  "permissions": {
    "allow": ["Bash(ls)", "Read(~/x)"],
    "deny": []
  },
  "trailing": true
}
"#;

#[test]
fn json_改已有值_只动那一段字节() {
    let f = file("settings.json", JSON_SRC);
    let out = edit::apply_edits(&f, &[set(&["model"], json!("claude-sonnet-4-5"))]).unwrap();
    assert_eq!(
        out,
        JSON_SRC.replace("claude-opus-4", "claude-sonnet-4-5"),
        "只应替换值本身，注释与缩进必须保持"
    );
}

#[test]
fn json_改嵌套值_保留注释() {
    let f = file("settings.json", JSON_SRC);
    let out = edit::apply_edits(
        &f,
        &[set(
            &["env", "ANTHROPIC_BASE_URL"],
            json!("https://new.example.com"),
        )],
    )
    .unwrap();
    assert!(out.contains("// 顶部注释"));
    assert!(out.contains("https://new.example.com"));
    assert!(!out.contains("https://api.example.com"));
}

#[test]
fn json_新增键_保持缩进与顺序() {
    let f = file("settings.json", JSON_SRC);
    let out = edit::apply_edits(&f, &[set(&["effortLevel"], json!("high"))]).unwrap();
    assert!(
        out.contains("  \"effortLevel\": \"high\""),
        "新键应继承同级缩进:\n{out}"
    );
    // 新键要插在最后一个键之后、闭合括号之前
    let idx_new = out.find("effortLevel").unwrap();
    let idx_close = out.rfind('}').unwrap();
    assert!(idx_new < idx_close);
    // 原有的键顺序不能变
    let i_model = out.find("\"model\"").unwrap();
    let i_trail = out.find("\"trailing\"").unwrap();
    assert!(i_model < i_trail);
}

#[test]
fn json_删除键_连带处理逗号() {
    let f = file("settings.json", JSON_SRC);
    let out = edit::apply_edits(&f, &[rm(&["permissions", "deny"])]).unwrap();
    assert!(!out.contains("\"deny\""));
    assert!(out.contains("\"allow\""), "相邻的键不能被误删:\n{out}");
    // 结果必须仍是合法 JSON
    let v: Value = parse_jsonc(&out);
    assert!(v["permissions"]["allow"].is_array());
}

#[test]
fn json_删除数组元素_其余下标不乱() {
    let f = file("settings.json", JSON_SRC);
    let out = edit::apply_edits(&f, &[rm(&["permissions", "allow", "0"])]).unwrap();
    let v: Value = parse_jsonc(&out);
    let arr = v["permissions"]["allow"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0], "Read(~/x)");
}

#[test]
fn json_删最后一个成员_不留多余逗号() {
    let src = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    let f = file("x.json", src);
    let out = edit::apply_edits(&f, &[rm(&["b"])]).unwrap();
    let v: Value = parse_jsonc(&out);
    assert_eq!(v["a"], 1);
    assert!(v.get("b").is_none());
}

// ────────────────────────────────────────────── TOML

const TOML_SRC: &str = r#"# Codex 的配置
model = "gpt-5-codex"
model_provider = "default"   # 行尾注释

[model_providers.default]
name = "OpenAI"
base_url = "https://api.openai.com/v1"
"#;

#[test]
fn toml_改标量_保留注释与行尾注释() {
    let f = file("config.toml", TOML_SRC);
    let out = edit::apply_edits(&f, &[set(&["model"], json!("gpt-5.1"))]).unwrap();
    assert!(out.contains("# Codex 的配置"), "前导注释不能丢:\n{out}");
    assert!(out.contains("# 行尾注释"), "行尾注释不能丢:\n{out}");
    assert!(out.contains("model = \"gpt-5.1\""));
    assert!(
        out.contains("[model_providers.default]"),
        "表头不能被破坏:\n{out}"
    );
}

#[test]
fn toml_改嵌套表里的值() {
    let f = file("config.toml", TOML_SRC);
    let out = edit::apply_edits(
        &f,
        &[set(
            &["model_providers", "default", "base_url"],
            json!("https://proxy/v1"),
        )],
    )
    .unwrap();
    assert!(
        out.contains("[model_providers.default]"),
        "表头不能变:\n{out}"
    );
    assert!(out.contains("https://proxy/v1"));
    assert!(!out.contains("https://api.openai.com/v1"));
    // 解析回来必须还是同一个结构
    let tv: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(
        tv["model_providers"]["default"]["base_url"].as_str(),
        Some("https://proxy/v1")
    );
    assert_eq!(
        tv["model_providers"]["default"]["name"].as_str(),
        Some("OpenAI")
    );
}

#[test]
fn toml_新增嵌套键() {
    let f = file("config.toml", TOML_SRC);
    let out = edit::apply_edits(
        &f,
        &[set(
            &["model_providers", "default", "env_key"],
            json!("OPENAI_API_KEY"),
        )],
    )
    .unwrap();
    assert!(out.contains("env_key = \"OPENAI_API_KEY\""), "{out}");
    let tv: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(
        tv["model_providers"]["default"]["env_key"].as_str(),
        Some("OPENAI_API_KEY")
    );
}

/// 父级还不存在时也要能新增（自动把缺失的中间表建出来）
#[test]
fn toml_新增嵌套键_父表不存在_自动建表() {
    let f = file("config.toml", TOML_SRC);
    let out = edit::apply_edits(&f, &[set(&["sandbox", "auto_allow_bash"], json!(true))]).unwrap();
    let tv: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(
        tv["sandbox"]["auto_allow_bash"].as_bool(),
        Some(true),
        "{out}"
    );
}

/// JSON 同理：父对象不存在时把缺失的层级建出来
#[test]
fn json_新增嵌套键_父对象不存在_自动建对象() {
    let f = file("settings.json", "{\n  \"model\": \"x\"\n}\n");
    let out = edit::apply_edits(&f, &[set(&["sandbox", "auto_allow_bash"], json!(true))]).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["sandbox"]["auto_allow_bash"].as_bool(),
        Some(true),
        "{out}"
    );
    // 原有的键不能动
    assert_eq!(v["model"].as_str(), Some("x"), "{out}");
}

/// YAML 同理
#[test]
fn yaml_新增嵌套键_父块不存在_自动建块() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["sandbox", "mode"], json!("ask"))]).unwrap();
    let v: Value = serde_yaml::from_str(&out).unwrap();
    assert_eq!(v["sandbox"]["mode"].as_str(), Some("ask"), "{out}");
    // 原有的键不能动
    assert_eq!(v["provider"].as_str(), Some("minimax"), "{out}");
}

// ────────────────────────────────────────────── YAML

const YAML_SRC: &str = r#"# Hermes 配置
provider: minimax
model: abab6.5s-chat
empty_key:
with_comment: old   # 这条注释不能丢

tools:
  - name: web_search
    enabled: true
  - name: code_exec
    enabled: false

allow:
  - "Bash(ls)"
  - "Read(~/x)"
"#;

#[test]
fn yaml_改标量_保留注释() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["model"], json!("abab7-chat"))]).unwrap();
    assert!(out.contains("# Hermes 配置"));
    assert!(out.contains("model: abab7-chat"));
    assert!(!out.contains("model: abab6.5s-chat"));
}

#[test]
fn yaml_改序列里对象的字段() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["tools", "1", "enabled"], json!(true))]).unwrap();
    assert!(
        out.contains("enabled: true"),
        "序列项里的字段应能改:\n{out}"
    );
    let yv: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
    assert_eq!(yv["tools"][1]["enabled"].as_bool(), Some(true));
    assert_eq!(yv["tools"][0]["enabled"].as_bool(), Some(true));
    assert_eq!(yv["tools"][1]["name"].as_str(), Some("code_exec"));
}

#[test]
fn yaml_改标量序列项() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["allow", "1"], json!("Read(~/docs)"))]).unwrap();
    let yv: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
    assert_eq!(yv["allow"][1].as_str(), Some("Read(~/docs)"));
    assert_eq!(yv["allow"][0].as_str(), Some("Bash(ls)"));
}

#[test]
fn yaml_改空值_补上分隔符() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["empty_key"], json!("filled"))]).unwrap();
    assert!(out.contains("empty_key: filled"), "{out}");
    let yv: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
    assert_eq!(yv["empty_key"].as_str(), Some("filled"));
}

#[test]
fn yaml_改有行尾注释的值_注释要留下() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[set(&["with_comment"], json!("new"))]).unwrap();
    assert!(out.contains("# 这条注释不能丢"), "{out}");
    let yv: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
    assert_eq!(yv["with_comment"].as_str(), Some("new"));
}

#[test]
fn yaml_删除序列项_整块删掉() {
    let f = file("config.yaml", YAML_SRC);
    let out = edit::apply_edits(&f, &[rm(&["tools", "0"])]).unwrap();
    let yv: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
    let arr = yv["tools"].as_sequence().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"].as_str(), Some("code_exec"));
}

// ────────────────────────────────────────────── 通用

#[test]
fn 写回后仍能被解析() {
    for (name, src) in [
        ("settings.json", JSON_SRC),
        ("config.toml", TOML_SRC),
        ("config.yaml", YAML_SRC),
    ] {
        let f = file(name, src);
        let out = edit::apply_edits(&f, &[set(&["dummy"], json!("x"))]).unwrap();
        // 新键插进去之后，文件必须还能被原解析器读回来
        match f.format {
            edit::ConfigFormat::Toml => {
                toml::from_str::<toml::Value>(&out).unwrap();
            }
            edit::ConfigFormat::Yaml => {
                serde_yaml::from_str::<serde_yaml::Value>(&out).unwrap();
            }
            _ => {
                parse_jsonc(&out);
            }
        }
    }
}

// ────────────────────────────────────── v2：数组插入 / 移动 / 密钥安全阀

/// 数组插入用的多行 JSON
const ARR_SRC: &str = r#"{
  "permissions": {
    "allow": [
      "Bash(ls)",
      "Bash(cat)",
      "Read(*)"
    ]
  }
}
"#;

fn arr_path() -> Vec<String> {
    vec!["permissions".into(), "allow".into()]
}

#[test]
fn json_数组插入_追加到末尾且保持多行排版() {
    let f = file("c.json", ARR_SRC);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Insert {
            path: arr_path(),
            index: 99,
            value: json!("Bash(new)"),
        }],
    )
    .unwrap();
    // 追加到最后一项后面，缩进对齐
    assert!(out.contains("      \"Bash(new)\""), "实际:\n{out}");
    // 仍然合法
    let v = parse_jsonc(&out);
    assert_eq!(v["permissions"]["allow"].as_array().unwrap().len(), 4);
    assert_eq!(v["permissions"]["allow"][3], json!("Bash(new)"));
}

#[test]
fn json_数组插入_插到指定位置() {
    let f = file("c.json", ARR_SRC);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Insert {
            path: arr_path(),
            index: 1,
            value: json!("Bash(mid)"),
        }],
    )
    .unwrap();
    let v = parse_jsonc(&out);
    let a = v["permissions"]["allow"].as_array().unwrap();
    assert_eq!(a[0], json!("Bash(ls)"));
    assert_eq!(a[1], json!("Bash(mid)"));
    assert_eq!(a[2], json!("Bash(cat)"));
    assert_eq!(a[3], json!("Read(*)"));
    // 不能多出逗号
    assert!(!out.contains(",\n      ,"), "实际:\n{out}");
}

#[test]
fn json_数组移动_顺序正确且不产生多余逗号() {
    let f = file("c.json", ARR_SRC);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Move {
            path: arr_path(),
            from: 0,
            to: 1,
        }],
    )
    .unwrap();
    let v = parse_jsonc(&out);
    let a = v["permissions"]["allow"].as_array().unwrap();
    assert_eq!(a[0], json!("Bash(cat)"));
    assert_eq!(a[1], json!("Bash(ls)"));
    assert_eq!(a[2], json!("Read(*)"));
    assert!(!out.contains(",\n      ,"), "实际:\n{out}");
}

#[test]
fn json_数组移动_移到末尾() {
    let f = file("c.json", ARR_SRC);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Move {
            path: arr_path(),
            from: 0,
            to: 2,
        }],
    )
    .unwrap();
    let a = parse_jsonc(&out)["permissions"]["allow"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(a[0], json!("Bash(cat)"));
    assert_eq!(a[1], json!("Read(*)"));
    assert_eq!(a[2], json!("Bash(ls)"));
}

/// 回归：删中间元素时，不能把下一个元素一起删掉
/// （曾经「尾逗号清理」对所有删除都执行，导致连带删掉后一个元素）
#[test]
fn json_删除数组中间元素_不连带删掉后一个() {
    let f = file("c.json", ARR_SRC);
    let out = edit::apply_edits(&f, &[rm(&["permissions", "allow", "0"])]).unwrap();
    let a = parse_jsonc(&out)["permissions"]["allow"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0], json!("Bash(cat)"));
    assert_eq!(a[1], json!("Read(*)"));
    assert!(!out.contains(",\n      ,"), "实际:\n{out}");
}

/// 回归：数组带尾逗号时，删最后一个元素要连带去掉尾逗号
#[test]
fn json_删除数组最后元素_尾逗号也去掉() {
    let src = "{\n  \"a\": [\n    \"x\",\n    \"y\",\n  ]\n}\n";
    let f = file("c.json", src);
    let out = edit::apply_edits(&f, &[rm(&["a", "1"])]).unwrap();
    assert!(!out.contains(",\n  ]"), "尾逗号没去掉:\n{out}");
    // 宽松解析器能读，严格 JSON 也该合法
    let strict: Result<Value, _> = serde_json::from_str(&out);
    assert!(strict.is_ok(), "结果不是合法 JSON:\n{out}");
}

/// 回归：密钥路径上的掩码值不能被当成真值写回
#[test]
fn 密钥掩码值_不会被写回文件() {
    let src = "{\n  \"env\": {\n    \"ANTHROPIC_API_KEY\": \"sk-ant-real-value\"\n  }\n}\n";
    let f = file("c.json", src);
    // 前端拿到的是脱敏后的掩码
    let masked = "sk-a****alue";
    let out = edit::apply_edits(&f, &[set(&["env", "ANTHROPIC_API_KEY"], json!(masked))]).unwrap();
    // 原值必须原封不动
    assert!(
        out.contains("sk-ant-real-value"),
        "掩码值不该写进文件:\n{out}"
    );
    assert!(!out.contains(masked), "掩码值写进了文件:\n{out}");
}

/// 回归：密钥路径上改**真值**时应该能写进去
#[test]
fn 密钥路径上改真值_能正常写回() {
    let src = "{\n  \"env\": {\n    \"ANTHROPIC_API_KEY\": \"sk-ant-old\"\n  }\n}\n";
    let f = file("c.json", src);
    let out = edit::apply_edits(
        &f,
        &[set(&["env", "ANTHROPIC_API_KEY"], json!("sk-ant-new"))],
    )
    .unwrap();
    assert!(out.contains("sk-ant-new"), "实际:\n{out}");
    assert!(!out.contains("sk-ant-old"), "实际:\n{out}");
}

/// 回归：非密钥字段叫 maxTokens 时不能被当成密钥（AI 配置里 token 也指计数）
#[test]
fn 非密钥字段_不会被误脱敏() {
    let src = "{\n  \"maxTokens\": 4096\n}\n";
    let f = file("c.json", src);
    let out = edit::apply_edits(&f, &[set(&["maxTokens"], json!(8192))]).unwrap();
    assert!(out.contains("8192"), "实际:\n{out}");
}

// ────────────────────────────────────── v2：TOML / YAML 的数组操作

const TOML_ARR: &str = r#"allow = [
  "Bash(ls)",
  "Bash(cat)",
]
"#;

const YAML_ARR: &str = "allow:\n  - Bash(ls)\n  - Bash(cat)\n";

#[test]
fn toml_数组插入_结果仍是合法toml() {
    let f = file("c.toml", TOML_ARR);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Insert {
            path: vec!["allow".into()],
            index: 1,
            value: json!("Bash(mid)"),
        }],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).expect("TOML 解析失败");
    let a = v["allow"].as_array().unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].as_str().unwrap(), "Bash(ls)");
    assert_eq!(a[1].as_str().unwrap(), "Bash(mid)");
    assert_eq!(a[2].as_str().unwrap(), "Bash(cat)");
}

#[test]
fn toml_数组移动_顺序正确() {
    let f = file("c.toml", TOML_ARR);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Move {
            path: vec!["allow".into()],
            from: 0,
            to: 1,
        }],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).expect("TOML 解析失败");
    let a = v["allow"].as_array().unwrap();
    assert_eq!(a[0].as_str().unwrap(), "Bash(cat)");
    assert_eq!(a[1].as_str().unwrap(), "Bash(ls)");
}

#[test]
fn yaml_数组插入_结果仍是合法yaml() {
    let f = file("c.yaml", YAML_ARR);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Insert {
            path: vec!["allow".into()],
            index: 1,
            value: json!("Bash(mid)"),
        }],
    )
    .unwrap();
    let v: serde_yaml::Value = serde_yaml::from_str(&out).expect("YAML 解析失败");
    let a = v["allow"].as_sequence().unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].as_str().unwrap(), "Bash(ls)");
    assert_eq!(a[1].as_str().unwrap(), "Bash(mid)");
    assert_eq!(a[2].as_str().unwrap(), "Bash(cat)");
}

#[test]
fn yaml_数组移动_顺序正确() {
    let f = file("c.yaml", YAML_ARR);
    let out = edit::apply_edits(
        &f,
        &[EditOp::Move {
            path: vec!["allow".into()],
            from: 0,
            to: 1,
        }],
    )
    .unwrap();
    let v: serde_yaml::Value = serde_yaml::from_str(&out).expect("YAML 解析失败");
    let a = v["allow"].as_sequence().unwrap();
    assert_eq!(a[0].as_str().unwrap(), "Bash(cat)");
    assert_eq!(a[1].as_str().unwrap(), "Bash(ls)");
}
