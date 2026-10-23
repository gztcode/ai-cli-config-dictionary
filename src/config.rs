//! 读取真实配置文件 → 统一成 serde_json::Value。
//!
//! 三种格式（JSON / TOML / YAML）统一成 Value 之后，
//! 校验逻辑就能一套通吃，不用为每家写一遍。

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};
use std::path::Path;

/// 按格式解析配置文本。返回统一 Value。
pub fn parse(text: &str, format: &str) -> Result<Value> {
    match format {
        "json" => serde_json::from_str(text).context("JSON 解析失败"),
        "jsonc" => {
            let stripped = strip_jsonc(text);
            serde_json::from_str(&stripped).context("JSONC 解析失败")
        }
        "toml" => {
            let tv: toml::Value = toml::from_str(text).context("TOML 解析失败")?;
            Ok(toml_to_json(&tv))
        }
        "yaml" => {
            let yv: serde_yaml::Value = serde_yaml::from_str(text).context("YAML 解析失败")?;
            Ok(yaml_to_json(&yv))
        }
        other => Err(anyhow!("不支持的配置格式: {other}")),
    }
}

/// 读取并解析文件。
pub fn read(path: &Path, format: &str) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("读取失败: {}", path.display()))?;
    parse(&text, format)
}

/// 去掉 JSONC 的行注释与块注释（保守处理，不动字符串内的内容）。
pub fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for c2 in chars.by_ref() {
                        if c2 == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for c2 in chars.by_ref() {
                        if prev == '*' && c2 == '/' {
                            break;
                        }
                        prev = c2;
                    }
                }
                _ => out.push('/'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// TOML → JSON。日期时间降级为字符串。
pub fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut m = Map::new();
            for (k, val) in t.iter() {
                m.insert(k.clone(), toml_to_json(val));
            }
            Value::Object(m)
        }
    }
}

/// YAML → JSON。非字符串键转成字符串。
pub fn yaml_to_json(v: &serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => Value::String(s.clone()),
        serde_yaml::Value::Sequence(s) => Value::Array(s.iter().map(yaml_to_json).collect()),
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m.iter() {
                let key = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                out.insert(key, yaml_to_json(val));
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(&t.value),
    }
}

/// 按点路径在 Value 中取值，支持数组下标。
pub fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('.') {
        if seg == "[]" || seg == "[*]" {
            continue;
        }
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => {
                let idx: usize = seg.parse().ok()?;
                a.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// 提取父路径（去掉最后一段）。
pub fn parent_path(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[..i],
        None => "",
    }
}
