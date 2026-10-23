//! JSON Schema → IR 转换器。
//!
//! 这里是原型最关键的一段：要证明「一份 JSON Schema 足以推导出
//! 含义 + 文档链接 + 类型 + 枚举 + 约束」，并且推导结果足以驱动校验。

use crate::ir::*;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`\)\]\},;]+"#).expect("valid url regex"))
}

/// 从任意文本中抽取 URL（去重、去尾部标点）。
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for m in url_re().find_iter(text) {
        let u = m
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '。', '，', '；', '：']);
        if !u.is_empty() && !out.iter().any(|x: &String| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

/// 跟随 `$ref` 展开 `$defs` 引用，并把兄弟字段合并回来。
fn resolve(sub: &Value, defs: &Map<String, Value>) -> Value {
    let mut cur = sub.clone();
    for _ in 0..8 {
        let r = match cur.get("$ref").and_then(|v| v.as_str()) {
            Some(r) => r.to_string(),
            None => break,
        };
        let name = match r.strip_prefix("#/$defs/") {
            Some(n) => n,
            None => break,
        };
        let target = match defs.get(name) {
            Some(t) => t.clone(),
            None => break,
        };
        let mut merged = target;
        if let Some(obj) = cur.as_object() {
            if let Some(m) = merged.as_object_mut() {
                for (k, v) in obj.iter() {
                    if k != "$ref" {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        cur = merged;
    }
    cur
}

fn prim_type(t: &str) -> ValueType {
    match t {
        "string" => ValueType::String,
        "number" => ValueType::Number,
        "integer" => ValueType::Integer,
        "boolean" => ValueType::Boolean,
        "object" => ValueType::Object,
        "array" => ValueType::Array,
        "null" => ValueType::Null,
        _ => ValueType::Any,
    }
}

/// 只取类型，不做分支归约。
fn detect_type_only(sub: &Value) -> Option<ValueType> {
    match sub.get("type") {
        Some(Value::String(t)) => Some(prim_type(t)),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(|v| v.as_str())
            .find(|s| *s != "null")
            .map(prim_type),
        _ => {
            if sub.get("properties").is_some() {
                Some(ValueType::Object)
            } else {
                None
            }
        }
    }
}

/// 把分支里的 properties / required 合并进 merged（已存在的键不覆盖）。
///
/// `merge_required` 的语义很关键：
/// - `allOf`：所有分支同时成立 → required 必须合并
/// - `anyOf` / `oneOf`：满足任一分支即可 → required **不能**合并，
///   否则会出现「command / prompt / url / server / tool 全都必填」这种误报
fn merge_props(merged: &mut Value, branch: &Value, merge_required: bool) {
    let Some(mo) = merged.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(props)) = branch.get("properties") {
        let entry = mo
            .entry("properties".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(t) = entry.as_object_mut() {
            for (k, v) in props {
                t.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    if !merge_required {
        return;
    }
    if let Some(Value::Array(req)) = branch.get("required") {
        let entry = mo
            .entry("required".to_string())
            .or_insert_with(|| Value::Array(vec![]));
        if let Some(t) = entry.as_array_mut() {
            for r in req {
                if !t.contains(r) {
                    t.push(r.clone());
                }
            }
        }
    }
}

/// 分支归约：把 `anyOf` / `oneOf` / `allOf` 折叠成单一有效 schema。
///
/// 这一步是必须的——Claude Code 的 hookCommand 就是
/// `anyOf: [{type:object, properties:{...}}, {...}]`，
/// properties 全在分支里，不归约就会整个丢掉。
///
/// 返回 (有效 schema, 枚举值, 类型覆盖, 是否可空)
fn reduce_branches(
    sub: &Value,
    defs: &Map<String, Value>,
) -> (Value, Vec<EnumValue>, Option<ValueType>, bool) {
    let mut merged = sub.clone();
    let mut enums: Vec<EnumValue> = Vec::new();
    let mut types: Vec<ValueType> = Vec::new();
    let mut nullable = false;
    let mut saw = false;

    for key in ["allOf", "anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = sub.get(key) else {
            continue;
        };
        saw = true;
        for b in branches {
            let rb = resolve(b, defs);
            if let Some(c) = rb.get("const") {
                enums.push(EnumValue {
                    value: c.clone(),
                    description: rb
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(String::from),
                });
                continue;
            }
            if let Some(Value::Array(items)) = rb.get("enum") {
                let dm = rb.get("x-enum-descriptions").and_then(|v| v.as_object());
                for it in items {
                    enums.push(EnumValue {
                        value: it.clone(),
                        description: dm
                            .and_then(|m| m.get(it.as_str().unwrap_or_default()))
                            .and_then(|d| d.as_str())
                            .map(String::from),
                    });
                }
                continue;
            }
            match detect_type_only(&rb) {
                Some(ValueType::Null) => nullable = true,
                Some(t) => types.push(t),
                None => {}
            }
            merge_props(&mut merged, &rb, key == "allOf");
        }
    }

    if !saw {
        return (merged, enums, None, false);
    }
    if let Some(m) = merged.as_object_mut() {
        m.remove("anyOf");
        m.remove("oneOf");
        m.remove("allOf");
    }
    let type_override = if types.is_empty() {
        None
    } else {
        let t = types[0];
        if let Some(m) = merged.as_object_mut() {
            m.insert("type".into(), Value::String(t.label().to_string()));
        }
        Some(t)
    };
    (merged, enums, type_override, nullable)
}

/// 从 `oneOf` / `anyOf` 分支里收集 const 型枚举。
fn const_enum_from(branches: &[Value]) -> Option<Vec<EnumValue>> {
    let mut vals = Vec::new();
    for b in branches {
        let b = b.clone();
        if let Some(c) = b.get("const") {
            vals.push(EnumValue {
                value: c.clone(),
                description: b
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(String::from)
                    .or_else(|| b.get("title").and_then(|d| d.as_str()).map(String::from)),
            });
        }
    }
    if vals.is_empty() {
        None
    } else {
        Some(vals)
    }
}

/// 判定类型 / 是否可空 / 枚举取值。
fn detect(sub: &Value) -> (ValueType, bool, Vec<EnumValue>) {
    // 0) 单值约束 const
    if let Some(c) = sub.get("const") {
        let nullable = c.is_null();
        return (
            ValueType::Enum,
            nullable,
            vec![EnumValue {
                value: c.clone(),
                description: sub
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(String::from),
            }],
        );
    }

    // 1) 直接的 enum
    if let Some(Value::Array(items)) = sub.get("enum") {
        let desc_map = sub.get("x-enum-descriptions").and_then(|v| v.as_object());
        let mut vals = Vec::new();
        let mut nullable = false;
        for it in items {
            if it.is_null() {
                nullable = true;
            }
            let desc = desc_map
                .and_then(|m| m.get(it.as_str().unwrap_or_default()))
                .and_then(|d| d.as_str())
                .map(String::from);
            vals.push(EnumValue {
                value: it.clone(),
                description: desc,
            });
        }
        return (ValueType::Enum, nullable, vals);
    }

    // 2) oneOf / anyOf
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(branches)) = sub.get(key) {
            if let Some(vals) = const_enum_from(branches) {
                let nullable = vals.iter().any(|v| v.value.is_null());
                return (ValueType::Enum, nullable, vals);
            }
            // 类型联合
            let mut nullable = false;
            let mut picked: Option<ValueType> = None;
            for b in branches {
                let t = b.get("type").and_then(|v| v.as_str());
                match t {
                    Some("null") => nullable = true,
                    Some(other) => {
                        if picked.is_none() {
                            picked = Some(prim_type(other));
                        }
                    }
                    None => {
                        if picked.is_none() {
                            picked = Some(ValueType::Any);
                        }
                    }
                }
            }
            let vt = picked.unwrap_or(ValueType::Any);
            return (vt, nullable, Vec::new());
        }
    }

    // 3) type 字段
    match sub.get("type") {
        Some(Value::String(t)) => return (prim_type(t), false, Vec::new()),
        Some(Value::Array(ts)) => {
            let mut nullable = false;
            let mut picked = ValueType::Any;
            for t in ts {
                if let Some(s) = t.as_str() {
                    if s == "null" {
                        nullable = true;
                    } else if picked == ValueType::Any {
                        picked = prim_type(s);
                    }
                }
            }
            return (picked, nullable, Vec::new());
        }
        _ => {}
    }

    // 4) 结构推断
    if sub.get("properties").is_some() || sub.get("patternProperties").is_some() {
        return (ValueType::Object, false, Vec::new());
    }
    if sub.get("additionalProperties").is_some() {
        return (ValueType::Object, false, Vec::new());
    }
    (ValueType::Any, false, Vec::new())
}

fn constraints_of(sub: &Value) -> Constraints {
    let f = |k: &str| sub.get(k).and_then(|v| v.as_f64());
    let u = |k: &str| sub.get(k).and_then(|v| v.as_u64());
    Constraints {
        minimum: f("minimum"),
        maximum: f("maximum"),
        min_length: u("minLength"),
        max_length: u("maxLength"),
        pattern: sub
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(String::from),
        format: sub.get("format").and_then(|v| v.as_str()).map(String::from),
        min_items: u("minItems"),
        max_items: u("maxItems"),
    }
}

/// 判断是否已废弃，并抽取废弃说明。
fn deprecation_of(sub: &Value, desc: Option<&str>) -> (bool, Option<String>) {
    if sub.get("deprecated").and_then(|v| v.as_bool()) == Some(true) {
        return (true, desc.map(String::from));
    }
    if let Some(d) = desc {
        for marker in ["DEPRECATED", "@deprecated", "已废弃", "Deprecated"] {
            if d.contains(marker) {
                // 取包含标记的第一句
                let note = d
                    .split('\n')
                    .find(|line| line.contains(marker))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| d.to_string());
                return (true, Some(note));
            }
        }
    }
    (false, None)
}

/// 把一个属性节点转成 IR。
fn convert_prop(
    name: &str,
    raw: &Value,
    defs: &Map<String, Value>,
    _required_names: &[String],
    is_required: bool,
    parent_path: &str,
    inherited: &[String],
) -> IrKey {
    let sub = resolve(raw, defs);
    // 分支归约：把 anyOf / oneOf / allOf 里的结构合并进来
    let (eff, branch_enums, type_override, branch_nullable) = reduce_branches(&sub, defs);
    let path = if parent_path.is_empty() {
        name.to_string()
    } else if name == "[]" || name == "[*]" {
        format!("{parent_path}{name}")
    } else {
        format!("{parent_path}.{name}")
    };

    let desc = eff
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let (mut value_type, mut nullable, mut enum_values) = detect(&eff);
    if !branch_enums.is_empty() {
        if eff.get("properties").is_none() {
            value_type = ValueType::Enum;
            enum_values = branch_enums;
        } else {
            enum_values.extend(branch_enums);
        }
    }
    if value_type == ValueType::Any {
        if let Some(t) = type_override {
            value_type = t;
        }
    }
    nullable = nullable || branch_nullable;

    // 文档链接：描述 + 常见扩展字段 + examples
    let mut doc_urls: Vec<String> = Vec::new();
    if let Some(d) = &desc {
        doc_urls.extend(extract_urls(d));
    }
    for key in [
        "x-doc-url",
        "x-docs-url",
        "docUrl",
        "docsUrl",
        "$comment",
        "x-url",
    ] {
        if let Some(u) = eff.get(key).and_then(|v| v.as_str()) {
            if u.starts_with("http") {
                doc_urls.extend(extract_urls(u));
            }
        }
    }
    if let Some(Value::Array(ex)) = eff.get("examples") {
        for e in ex {
            if let Some(s) = e.as_str() {
                doc_urls.extend(extract_urls(s));
            }
        }
    }
    doc_urls.sort();
    doc_urls.dedup();

    let (deprecated, deprecated_note) = deprecation_of(&eff, desc.as_deref());
    let constraints = constraints_of(&eff);

    let has_props = eff.get("properties").is_some();
    let add_props = eff.get("additionalProperties");
    let free_form = add_props
        .map(|v| v == &Value::Bool(true) || v.is_object())
        .unwrap_or(false)
        || eff.get("patternProperties").is_some();

    let kind = if has_props {
        KeyKind::Object
    } else if value_type == ValueType::Array {
        KeyKind::Array
    } else {
        KeyKind::Leaf
    };

    // 子键
    let mut children = Vec::new();
    if has_props {
        let child_required: Vec<String> = eff
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(props) = eff.get("properties").and_then(|v| v.as_object()) {
            for (cname, craw) in props.iter() {
                let creq = child_required.iter().any(|r| r == cname);
                children.push(convert_prop(
                    cname,
                    craw,
                    defs,
                    &child_required,
                    creq,
                    &path,
                    &doc_urls,
                ));
            }
        }
    }
    // 数组元素 schema
    if let Some(items) = eff.get("items") {
        if items.is_object() {
            children.push(convert_prop(
                "[]",
                items,
                defs,
                &[],
                false,
                &path,
                &doc_urls,
            ));
        }
    }
    // 自由键 schema
    if let Some(ap) = add_props {
        if ap.is_object() {
            children.push(convert_prop("[*]", ap, defs, &[], false, &path, &doc_urls));
        }
    }

    IrKey {
        path,
        name: name.to_string(),
        kind,
        value_type,
        nullable,
        description: desc,
        // 父级没有自己的锚点时，子键继承父级的 —— 父级那一条文档链接
        // 讲的就是整棵子树（例如 claude-code 的 `permissions` 一节涵盖所有子项）。
        doc_urls: if doc_urls.is_empty() {
            inherited.to_vec()
        } else {
            doc_urls
        },
        enum_values,
        default: eff.get("default").cloned(),
        examples: eff
            .get("examples")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        deprecated,
        deprecated_note,
        required: is_required,
        constraints,
        free_form,
        children,
        translations: BTreeMap::new(),
    }
}

/// 把整份 schema 转成根级 IR 键列表。
pub fn convert(schema: &Value) -> Vec<IrKey> {
    let defs = schema
        .get("$defs")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let props = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for (name, sub) in props.iter() {
        // 跳过 JSON Schema 自身的元键（$schema / $id 等），它们不是配置项
        if name.starts_with('$') {
            continue;
        }
        let is_req = required.iter().any(|r| r == name);
        out.push(convert_prop(name, sub, &defs, &required, is_req, "", &[]));
    }
    out
}

/// 统计 IR 质量指标。
pub fn stats(keys: &[IrKey]) -> IrStats {
    let mut s = IrStats::default();
    fn walk(keys: &[IrKey], s: &mut IrStats) {
        for k in keys {
            s.total_keys += 1;
            let is_leaf = k.children.is_empty();
            if is_leaf {
                s.leaf_keys += 1;
                s.leaf_total += 1;
            }
            if k.description.is_some() {
                s.documented += 1;
                if is_leaf {
                    s.leaf_documented += 1;
                }
            }
            if !k.doc_urls.is_empty() {
                s.with_doc_url += 1;
                if is_leaf {
                    s.leaf_with_doc_url += 1;
                }
            }
            if !k.enum_values.is_empty() {
                s.with_enum += 1;
                if is_leaf {
                    s.leaf_with_enum += 1;
                }
            }
            if k.default.is_some() {
                s.with_default += 1;
            }
            if k.deprecated {
                s.deprecated += 1;
            }
            if !k.translations.is_empty() {
                s.translated += 1;
            }
            walk(&k.children, s);
        }
    }
    walk(keys, &mut s);
    s
}
