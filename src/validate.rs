//! 用 IR 校验真实配置。
//!
//! 注意：校验器跑在 IR 上而不是原始 JSON Schema 上。
//! 这本身就是一项验证——说明 IR 携带的信息足够完成校验，
//! GUI 完全可以只拿 IR 就实现「保存前拦截错误」。

use crate::ir::*;
use serde_json::Value;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warning,
}

impl Level {
    pub fn label(&self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warn",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Issue {
    pub path: String,
    pub level: Level,
    pub code: String,
    pub message: String,
}

/// 单个配置文件的校验结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileReport {
    pub path: String,
    pub exists: bool,
    pub parsed: bool,
    pub error: Option<String>,
    pub issues: Vec<Issue>,
    pub key_count: usize,
}

impl FileReport {
    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.level == Level::Error)
            .count()
    }
    pub fn warn_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.level == Level::Warning)
            .count()
    }
}

/// JSON 值的类型名，用于错误提示。
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(vt: ValueType, v: &Value) -> bool {
    match vt {
        ValueType::String => v.is_string(),
        ValueType::Number => v.is_number(),
        ValueType::Integer => v.is_i64() || v.is_u64(),
        ValueType::Boolean => v.is_boolean(),
        ValueType::Object => v.is_object(),
        ValueType::Array => v.is_array(),
        ValueType::Enum => true, // 单独判断
        ValueType::Any => true,
        ValueType::Null => v.is_null(),
    }
}

/// 递归校验一个值与对应的 IR 节点。
pub fn validate_value(value: &Value, key: &IrKey, path: &str, issues: &mut Vec<Issue>) {
    // null 且允许 null → 直接放行
    if value.is_null() {
        if key.nullable {
            return;
        }
        issues.push(Issue {
            path: path.to_string(),
            level: Level::Error,
            code: "type".into(),
            message: "不允许为 null".into(),
        });
        return;
    }

    // 类型
    if !type_matches(key.value_type, value) {
        issues.push(Issue {
            path: path.to_string(),
            level: Level::Error,
            code: "type".into(),
            message: format!(
                "类型错误：期望 {}，实际 {}",
                key.value_type.label(),
                json_type_name(value)
            ),
        });
        return; // 类型都对不上，递归下去没有意义
    }

    // 枚举
    if key.value_type == ValueType::Enum && !key.enum_values.is_empty() {
        let allowed = &key.enum_values;
        let ok = allowed.iter().any(|e| e.value == *value);
        if !ok {
            let names: Vec<String> = allowed
                .iter()
                .map(|e| match &e.value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            issues.push(Issue {
                path: path.to_string(),
                level: Level::Error,
                code: "enum".into(),
                message: format!("取值不在允许范围内，允许值：{}", names.join(" | ")),
            });
        }
    }

    // 标量约束
    let c = &key.constraints;
    if let Value::String(s) = value {
        if let Some(min) = c.min_length {
            if (s.chars().count() as u64) < min {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "length".into(),
                    message: format!("长度不足，最小 {min}"),
                });
            }
        }
        if let Some(max) = c.max_length {
            if (s.chars().count() as u64) > max {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "length".into(),
                    message: format!("长度超限，最大 {max}"),
                });
            }
        }
        if let Some(p) = &c.pattern {
            let re = regex_cache(p);
            match re {
                Some(re) => {
                    if !re.is_match(s) {
                        issues.push(Issue {
                            path: path.to_string(),
                            level: Level::Error,
                            code: "pattern".into(),
                            message: format!("不匹配要求的格式 /{p}/"),
                        });
                    }
                }
                None => issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Warning,
                    code: "pattern".into(),
                    message: format!("schema 中的正则无法编译：/{p}/"),
                }),
            }
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = c.minimum {
            if n < min {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "range".into(),
                    message: format!("取值过小，最小 {min}"),
                });
            }
        }
        if let Some(max) = c.maximum {
            if n > max {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "range".into(),
                    message: format!("取值过大，最大 {max}"),
                });
            }
        }
    }
    if let Value::Array(a) = value {
        if let Some(min) = c.min_items {
            if (a.len() as u64) < min {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "items".into(),
                    message: format!("元素数量不足，最小 {min}"),
                });
            }
        }
        if let Some(max) = c.max_items {
            if (a.len() as u64) > max {
                issues.push(Issue {
                    path: path.to_string(),
                    level: Level::Error,
                    code: "items".into(),
                    message: format!("元素数量超限，最大 {max}"),
                });
            }
        }
    }

    // 递归子键
    if value.is_object() {
        let obj = value.as_object().unwrap();
        let known: Vec<&str> = key
            .children
            .iter()
            .filter(|c| c.name != "[]" && c.name != "[*]")
            .map(|c| c.name.as_str())
            .collect();

        for child in &key.children {
            let child_path = if path.is_empty() {
                child.name.clone()
            } else {
                format!("{path}.{}", child.name)
            };
            match obj.get(&child.name) {
                Some(cv) => validate_value(cv, child, &child_path, issues),
                None => {
                    if child.required {
                        issues.push(Issue {
                            path: child_path,
                            level: Level::Error,
                            code: "required".into(),
                            message: "缺少必填项".into(),
                        });
                    }
                }
            }
        }

        // 未知键：schema 里没有、也不在自由键区域
        if !key.free_form {
            for k in obj.keys() {
                if !known.contains(&k.as_str()) {
                    issues.push(Issue {
                        path: if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        },
                        level: Level::Warning,
                        code: "unknown".into(),
                        message: "schema 中不存在该配置项（可能是自定义/实验性选项）".into(),
                    });
                }
            }
        }
    }

    // 数组元素
    if value.is_array() {
        if let Some(items_key) = key.children.iter().find(|c| c.name == "[]") {
            if let Some(arr) = value.as_array() {
                for (i, el) in arr.iter().enumerate() {
                    let p = format!("{path}[{i}]");
                    validate_value(el, items_key, &p, issues);
                }
            }
        }
    }
}

/// 顶层入口：拿一份配置去比对一棵 IR 树。
pub fn validate_config(root: &Value, keys: &[IrKey], file_path: &str) -> FileReport {
    let mut issues = Vec::new();
    let obj = root.as_object();
    if let Some(obj) = obj {
        // 根级未知键
        let known: Vec<&str> = keys.iter().map(|k| k.name.as_str()).collect();
        for (k, v) in obj {
            if !known.contains(&k.as_str()) {
                issues.push(Issue {
                    path: k.clone(),
                    level: Level::Warning,
                    code: "unknown".into(),
                    message: "schema 中不存在该配置项".into(),
                });
            }
            let _ = v;
        }
    }
    for k in keys {
        match obj.and_then(|o| o.get(&k.name)) {
            Some(v) => validate_value(v, k, &k.name, &mut issues),
            None => {
                if k.required {
                    issues.push(Issue {
                        path: k.name.clone(),
                        level: Level::Error,
                        code: "required".into(),
                        message: "缺少必填项".into(),
                    });
                }
            }
        }
    }
    let key_count = count_keys(root);
    FileReport {
        path: file_path.to_string(),
        exists: true,
        parsed: true,
        error: None,
        issues,
        key_count,
    }
}

fn count_keys(v: &Value) -> usize {
    match v {
        Value::Object(m) => m.len() + m.values().map(count_keys).sum::<usize>(),
        Value::Array(a) => a.iter().map(count_keys).sum(),
        _ => 0,
    }
}

/// 正则缓存，避免同一条 pattern 反复编译。
pub fn regex_cache(pattern: &str) -> Option<regex::Regex> {
    static CACHE: OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<regex::Regex>>>,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut g = cache.lock().ok()?;
    if let Some(v) = g.get(pattern) {
        return v.clone();
    }
    let compiled = regex::Regex::new(pattern).ok();
    g.insert(pattern.to_string(), compiled.clone());
    compiled
}
