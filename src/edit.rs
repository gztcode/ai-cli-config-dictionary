//! 保格式的配置编辑。
//!
//! 核心原则：**只改动用户指定的那几个字节**，其余部分（缩进、注释、键顺序、
//! YAML anchor、空行）一律原样保留。
//!
//! - JSON / JSONC → 自写的 `jsonc` 解析器（带字节偏移），精确到字节替换
//! - TOML        → `toml_edit`，天然保格式
//! - YAML        → 行级定点替换（无成熟保格式库），尽力而为

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use toml_edit::{value as toml_value, DocumentMut, Item, Table};

use crate::jsonc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Jsonc,
    Toml,
    Yaml,
}

impl ConfigFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ConfigFormat::Json => "JSON",
            ConfigFormat::Jsonc => "JSONC",
            ConfigFormat::Toml => "TOML",
            ConfigFormat::Yaml => "YAML",
        }
    }
}

/// 一条编辑操作
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// 设置（存在则改，不存在则加）
    Set { path: Vec<String>, value: Value },
    /// 删除
    Remove { path: Vec<String> },
    /// 在数组的指定位置插入元素（`path` 指向数组本身）
    Insert {
        path: Vec<String>,
        index: usize,
        value: Value,
    },
    /// 移动数组元素（`path` 指向数组本身）
    Move {
        path: Vec<String>,
        from: usize,
        to: usize,
    },
}

impl EditOp {
    /// 这条操作作用在哪个路径上
    pub fn path(&self) -> &[String] {
        match self {
            EditOp::Set { path, .. } => path,
            EditOp::Remove { path } => path,
            EditOp::Insert { path, .. } => path,
            EditOp::Move { path, .. } => path,
        }
    }
}

/// 载入后的文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedFile {
    pub path: String,
    pub format: ConfigFormat,
    /// 原始文本
    pub text: String,
    /// 解析后的值（用于展示）
    pub value: Value,
    /// 是否是新文件（不存在时创建）
    pub is_new: bool,
    /// 内容指纹（sha256 前 16 位），用于检测外部改动
    pub fingerprint: String,
}

/// 计算内容指纹
pub fn fingerprint(text: &str) -> String {
    crate::registry::sha256_hex(text.as_bytes())[..16].to_string()
}

/// 按扩展名 + 内容判断格式
pub fn detect_format(path: &Path, text: &str) -> ConfigFormat {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if name.ends_with(".toml") {
        return ConfigFormat::Toml;
    }
    if name.ends_with(".yaml") || name.ends_with(".yml") {
        return ConfigFormat::Yaml;
    }
    if name.ends_with(".jsonc") {
        return ConfigFormat::Jsonc;
    }
    if name.ends_with(".json") {
        let t = text.trim_start();
        if t.contains("//") || t.contains("/*") || t.contains(",}") || t.contains(",]") {
            return ConfigFormat::Jsonc;
        }
        return ConfigFormat::Json;
    }
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        if t.contains("//") || t.contains(",}") {
            ConfigFormat::Jsonc
        } else {
            ConfigFormat::Json
        }
    } else if t.contains('=') && !t.contains(':') {
        ConfigFormat::Toml
    } else {
        ConfigFormat::Yaml
    }
}

/// 读取并解析
pub fn load(path: &Path) -> Result<LoadedFile> {
    if !path.exists() {
        return Ok(LoadedFile {
            path: path.display().to_string(),
            format: detect_format(path, ""),
            text: String::new(),
            value: Value::Object(serde_json::Map::new()),
            is_new: true,
            fingerprint: fingerprint(""),
        });
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("读取失败: {}", path.display()))?;
    let format = detect_format(path, &text);
    let value =
        parse_value(&text, format).with_context(|| format!("解析失败: {}", path.display()))?;
    Ok(LoadedFile {
        path: path.display().to_string(),
        format,
        fingerprint: fingerprint(&text),
        text,
        value,
        is_new: false,
    })
}

/// 按格式解析成统一的 Value
pub fn parse_value(text: &str, format: ConfigFormat) -> Result<Value> {
    match format {
        ConfigFormat::Json | ConfigFormat::Jsonc => Ok(jsonc::parse(text)?.value),
        ConfigFormat::Toml => {
            let tv: toml::Value = toml::from_str(text).context("TOML 解析失败")?;
            Ok(crate::config::toml_to_json(&tv))
        }
        ConfigFormat::Yaml => {
            let yv: serde_yaml::Value = serde_yaml::from_str(text).context("YAML 解析失败")?;
            Ok(crate::config::yaml_to_json(&yv))
        }
    }
}

// ---------------------------------------------------------------- 应用编辑

/// 在已载入的文件上应用编辑，返回新的文本
pub fn apply_edits(file: &LoadedFile, ops: &[EditOp]) -> Result<String> {
    // 写回安全阀：密钥路径上的「掩码值」不是真值，不能写进文件。
    // 这条操作被丢弃，等价于「用户没动这个字段」。
    let effective: Vec<&EditOp> = ops.iter().filter(|op| !is_masked_secret_op(op)).collect();
    let ops: Vec<EditOp> = effective.into_iter().cloned().collect();

    match file.format {
        ConfigFormat::Json | ConfigFormat::Jsonc => apply_json(&file.text, &ops),
        ConfigFormat::Toml => apply_toml(&file.text, &ops),
        ConfigFormat::Yaml => apply_yaml(&file.text, &ops),
    }
}

/// 这条操作是否是一条「把脱敏掩码当成真值写回」的误操作
fn is_masked_secret_op(op: &EditOp) -> bool {
    match op {
        EditOp::Set { path, value } => {
            crate::secret::is_secret_path(path)
                && matches!(value, Value::String(s) if crate::secret::is_masked(s))
        }
        _ => false,
    }
}

/// 把 Value 序列化成该格式的标量字面量
fn scalar_literal(v: &Value, format: ConfigFormat) -> String {
    match v {
        Value::String(s) => match format {
            ConfigFormat::Json | ConfigFormat::Jsonc => {
                serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
            }
            ConfigFormat::Toml => format!("{:?}", s),
            ConfigFormat::Yaml => {
                if s.chars().any(|c| ":#{}[]&*!|>'\"%@`,".contains(c))
                    || s.trim() != s
                    || s.is_empty()
                    || s.parse::<f64>().is_ok()
                {
                    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
                } else {
                    s.clone()
                }
            }
        },
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".into(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ------------------------------------------------------------------ JSONC

fn apply_json(text: &str, ops: &[EditOp]) -> Result<String> {
    let mut current = text.to_string();
    for op in ops {
        current = match op {
            EditOp::Set { path, value } => set_json(&current, path, value)?,
            EditOp::Remove { path } => remove_json(&current, path)?,
            EditOp::Insert { path, index, value } => {
                let lit = serde_json::to_string(value)?;
                insert_json_raw(&current, path, *index, &lit)?
            }
            EditOp::Move { path, from, to } => move_json(&current, path, *from, *to)?,
        };
    }
    Ok(current)
}

/// 在数组的 `index` 处插入一段字面量文本。
///
/// 保持原数组的排版：多行数组继续多行，单行数组继续单行；
/// 其余元素的文本一个字节都不动。
fn insert_json_raw(text: &str, path: &[String], index: usize, raw: &str) -> Result<String> {
    let root = jsonc::parse(text)?;
    let arr = jsonc::find(&root, path).ok_or_else(|| anyhow!("路径不存在: {}", path.join(".")))?;
    if arr.kind != jsonc::JKind::Array {
        return Err(anyhow!("目标不是数组: {}", path.join(".")));
    }

    let multiline = text[arr.start..arr.end].contains('\n');
    let n = arr.items.len();

    // 空数组：撑成多行
    if n == 0 {
        let indent = format!("{}  ", line_indent_at(text, arr.start));
        let close = line_indent_at(text, arr.start);
        let mut out = String::with_capacity(text.len() + raw.len() + 8);
        out.push_str(&text[..arr.start + 1]);
        out.push_str(&format!("\n{indent}{raw}\n{close}"));
        out.push_str(&text[arr.start + 1..]);
        return Ok(out);
    }

    let indent = line_indent_at(text, arr.items[0].start);
    let (at, content) = if index == 0 {
        if multiline {
            (arr.items[0].start, format!("{raw},\n{indent}"))
        } else {
            (arr.items[0].start, format!("{raw}, "))
        }
    } else if index < n {
        // 插到前一个元素之后
        let prev_end = arr.items[index - 1].end;
        if multiline {
            (prev_end, format!(",\n{indent}{raw}"))
        } else {
            (prev_end, format!(", {raw}"))
        }
    } else {
        // 追加到末尾
        let last_end = arr.items[n - 1].end;
        if multiline {
            (last_end, format!(",\n{indent}{raw}"))
        } else {
            (last_end, format!(", {raw}"))
        }
    };

    let mut out = String::with_capacity(text.len() + content.len());
    out.push_str(&text[..at]);
    out.push_str(&content);
    out.push_str(&text[at..]);
    Ok(out)
}

/// 移动数组元素：先摘出来，再按目标下标插回去。
///
/// 目标下标是**最终数组**里的位置，所以摘除之后不需要再做偏移修正。
fn move_json(text: &str, path: &[String], from: usize, to: usize) -> Result<String> {
    let root = jsonc::parse(text)?;
    let arr = jsonc::find(&root, path).ok_or_else(|| anyhow!("路径不存在: {}", path.join(".")))?;
    if arr.kind != jsonc::JKind::Array {
        return Err(anyhow!("目标不是数组: {}", path.join(".")));
    }
    let n = arr.items.len();
    if from >= n {
        return Err(anyhow!("源下标越界: {from}（共 {n} 项）"));
    }
    if to > n - 1 {
        return Err(anyhow!("目标下标越界: {to}（共 {n} 项）"));
    }
    if from == to {
        return Ok(text.to_string());
    }

    // 摘出原始文本（保留元素内部排版）
    let item = &arr.items[from];
    let raw = text[item.start..item.end].to_string();

    // 删除该元素（连带逗号）
    let mut item_path = path.to_vec();
    item_path.push(from.to_string());
    let shrunk = remove_json(text, &item_path)?;

    // 插到目标位置
    insert_json_raw(&shrunk, path, to, &raw)
}

/// 替换已有值；不存在则插入到父对象末尾
fn set_json(text: &str, path: &[String], value: &Value) -> Result<String> {
    if path.is_empty() {
        return Ok(text.to_string());
    }
    let root = jsonc::parse(text)?;
    let new_lit = serde_json::to_string(value)?;

    // 1) 已存在 → 就地替换值文本
    if let Some(node) = jsonc::find(&root, path) {
        let (s, e) = (node.start, node.end);
        let mut out = String::with_capacity(text.len() + new_lit.len());
        out.push_str(&text[..s]);
        out.push_str(&new_lit);
        out.push_str(&text[e..]);
        return Ok(out);
    }

    // 2) 不存在 → 插到父对象里
    let parent_path = &path[..path.len() - 1];
    let last_key = &path[path.len() - 1];

    // 父路径也可能不存在（往没有 `sandbox` 对象里加 `sandbox.x`）。
    // TOML 那边会自动建表，JSON 这边也要能建：找到最浅的、确实存在且是对象的
    // 祖先，把缺失的层级构造成嵌套对象一次性插进去。
    let (parent, missing): (&jsonc::JNode, &[String]) = match jsonc::find(&root, parent_path) {
        Some(n) if n.kind == jsonc::JKind::Object => (n, &[]),
        Some(_) => return Err(anyhow!("父节点不是对象，无法插入")),
        None => {
            let mut hit = None;
            let mut anchor = parent_path.len();
            while anchor > 0 {
                if let Some(n) = jsonc::find(&root, &parent_path[..anchor]) {
                    if n.kind == jsonc::JKind::Object {
                        hit = Some((n, anchor));
                        break;
                    }
                }
                anchor -= 1;
            }
            match hit {
                Some((n, a)) => (n, &parent_path[a..]),
                None => {
                    // 根对象就是最浅的祖先，缺失的层级全都要建
                    if root.kind != jsonc::JKind::Object {
                        return Err(anyhow!("父节点不是对象，无法插入"));
                    }
                    (&root, parent_path)
                }
            }
        }
    };

    // 要插入的成员：缺失的层级 + 末段键，包成嵌套对象
    let entry_key: String = if missing.is_empty() {
        last_key.clone()
    } else {
        missing[0].clone()
    };
    let new_lit = if missing.is_empty() {
        new_lit.clone()
    } else {
        // 由内往外包：{"b": {"c": value}}
        let mut nested = serde_json::to_value(value)?;
        for seg in missing[1..].iter().rev() {
            let mut m = serde_json::Map::new();
            m.insert(seg.clone(), nested);
            nested = Value::Object(m);
        }
        let mut m = serde_json::Map::new();
        m.insert(last_key.clone(), nested);
        serde_json::to_string(&Value::Object(m))?
    };

    let entry = format!("{}: {new_lit}", serde_json::to_string(&entry_key)?);
    let indent = parent
        .members
        .last()
        .map(|m| line_indent_at(text, m.start))
        .unwrap_or_else(|| format!("{}  ", line_indent_at(text, parent.start)));

    let (at, content) = match parent.members.last() {
        Some(m) => {
            // 值的后面可能跟着行尾注释（`"a": 1 // note`），要把新键放到注释之后
            let line_end = text[m.end..]
                .find('\n')
                .map(|i| m.end + i)
                .unwrap_or(text.len());
            let rest = text[m.end..line_end].trim();
            if rest.is_empty() {
                (m.end, format!(",\n{indent}{entry}"))
            } else {
                // 同一行有注释：先给原值补逗号，再把新键插到下一行
                let mut out = String::with_capacity(text.len() + entry.len() + 8);
                out.push_str(&text[..m.end]);
                out.push(',');
                out.push_str(&text[m.end..]);
                let at2 = if line_end < text.len() {
                    line_end + 1
                } else {
                    text.len()
                };
                out.insert_str(at2, &format!("{indent}{entry}\n"));
                return Ok(out);
            }
        }
        // 空对象 `{ }` → 撑成多行
        None => (
            parent.start + 1,
            format!("\n{indent}{entry}\n{}", line_indent_at(text, parent.start)),
        ),
    };

    let mut out = String::with_capacity(text.len() + content.len());
    out.push_str(&text[..at]);
    out.push_str(&content);
    out.push_str(&text[at..]);
    Ok(out)
}

/// 取某一字节位置所在行的前导空白
fn line_indent_at(text: &str, byte: usize) -> String {
    let bytes = text.as_bytes();
    let mut i = byte.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    let mut s = String::new();
    for &b in &bytes[i..byte.min(bytes.len())] {
        if b == b' ' || b == b'\t' {
            s.push(b as char);
        } else {
            break;
        }
    }
    s
}

/// 删除一个成员（连带处理逗号）
fn remove_json(text: &str, path: &[String]) -> Result<String> {
    if path.is_empty() {
        return Ok(text.to_string());
    }
    let root = jsonc::parse(text)?;
    let parent_path = &path[..path.len() - 1];
    let last_key = &path[path.len() - 1];
    let parent = jsonc::find(&root, parent_path)
        .ok_or_else(|| anyhow!("父路径不存在: {}", parent_path.join(".")))?;

    let (cut_s, cut_e) = match parent.kind {
        jsonc::JKind::Object => {
            let Some(pos) = parent.members.iter().position(|m| m.key == *last_key) else {
                return Ok(text.to_string());
            };
            let m = &parent.members[pos];
            if pos + 1 < parent.members.len() {
                // 删自己 + 后面的逗号
                (m.start, parent.members[pos + 1].start)
            } else if pos > 0 {
                // 删自己和前面的逗号
                (parent.members[pos - 1].end, m.end)
            } else {
                (m.start, m.end)
            }
        }
        jsonc::JKind::Array => {
            let idx: usize = last_key
                .parse()
                .map_err(|_| anyhow!("数组下标不是数字: {last_key}"))?;
            let item = parent
                .items
                .get(idx)
                .ok_or_else(|| anyhow!("数组下标越界: {idx}"))?;
            let is_last = idx + 1 >= parent.items.len();
            let (s, mut e) = if !is_last {
                // 删自己 + 后面的逗号与空白（到下一个元素开头）
                (item.start, parent.items[idx + 1].start)
            } else if idx > 0 {
                // 删自己和前面的逗号
                (parent.items[idx - 1].end, item.end)
            } else {
                // 唯一一项
                (item.start, item.end)
            };
            // 只有删的是**最后一个**元素时，数组尾逗号才需要连带去掉，
            // 否则会留下非法 JSON（删中间元素时那个逗号本来就在删除范围内）。
            if is_last {
                if let Some(p) = text[e..parent.end].find(',') {
                    e += p + 1;
                }
            }
            (s, e)
        }
        _ => return Err(anyhow!("父节点不是对象或数组")),
    };

    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..cut_s]);
    out.push_str(&text[cut_e..]);
    Ok(out)
}

// ------------------------------------------------------------------- TOML

fn apply_toml(text: &str, ops: &[EditOp]) -> Result<String> {
    let mut doc: DocumentMut = text.parse().map_err(|e| anyhow!("TOML 解析失败: {e}"))?;

    for op in ops {
        match op {
            EditOp::Set { path, value } => {
                if path.is_empty() {
                    continue;
                }
                let v = toml_value(json_to_toml_value(value)?);
                toml_set(doc.as_table_mut(), path, v)?;
            }
            EditOp::Remove { path } => {
                if path.is_empty() {
                    continue;
                }
                toml_remove(doc.as_table_mut(), path);
            }
            EditOp::Insert { path, index, value } => {
                toml_insert(doc.as_table_mut(), path, *index, value)?;
            }
            EditOp::Move { path, from, to } => {
                toml_move(doc.as_table_mut(), path, *from, *to)?;
            }
        }
    }
    Ok(doc.to_string())
}

/// TOML 里可作为数组操作目标的两种容器
enum TomlArray<'a> {
    /// `key = [1, 2, 3]`
    Values(&'a mut toml_edit::Array),
    /// `[[key]]`
    Tables(&'a mut toml_edit::ArrayOfTables),
}

/// 沿「表 → 表」路径下钻，找到最后一段指向的数组
///
/// 只支持表层级下钻；数组元素内部再嵌套（内联表）暂不支持，
/// 这种情况会明确报错而不是静默写错位置。
fn toml_array_mut<'a>(table: &'a mut Table, path: &[String]) -> Result<TomlArray<'a>> {
    let (head, rest) = path
        .split_first()
        .ok_or_else(|| anyhow!("路径为空，无法定位数组"))?;
    let child = table
        .get_mut(head)
        .ok_or_else(|| anyhow!("{head} 不存在"))?;
    if rest.is_empty() {
        return match child {
            Item::Value(toml_edit::Value::Array(a)) => Ok(TomlArray::Values(a)),
            Item::ArrayOfTables(a) => Ok(TomlArray::Tables(a)),
            _ => Err(anyhow!("{head} 不是数组")),
        };
    }
    match child {
        Item::Table(t) => toml_array_mut(t, rest),
        _ => Err(anyhow!(
            "{head} 不是表（数组元素内部的嵌套暂不支持，请改用整块编辑）"
        )),
    }
}

/// 在 TOML 数组里插入一个元素
fn toml_insert(table: &mut Table, path: &[String], index: usize, value: &Value) -> Result<()> {
    match toml_array_mut(table, path)? {
        TomlArray::Values(arr) => {
            let v = json_to_toml_value(value)?;
            let idx = index.min(arr.len());
            arr.insert(idx, v);
            Ok(())
        }
        TomlArray::Tables(arr) => {
            // 表数组只能插入一张表
            let v = json_to_toml_value(value)?;
            let toml_edit::Value::InlineTable(t) = v else {
                return Err(anyhow!("[[表数组]] 只能插入对象"));
            };
            let mut t = Table::from_iter(t.iter().map(|(k, v)| (k.to_string(), v.clone())));
            t.set_implicit(true);
            // toml_edit 的表数组没有 insert，克隆重建
            let mut items: Vec<Table> = arr.iter().cloned().collect();
            let idx = index.min(items.len());
            items.insert(idx, t);
            arr.clear();
            for it in items {
                arr.push(it);
            }
            Ok(())
        }
    }
}

/// 在 TOML 数组里移动元素
fn toml_move(table: &mut Table, path: &[String], from: usize, to: usize) -> Result<()> {
    match toml_array_mut(table, path)? {
        TomlArray::Values(arr) => {
            let n = arr.len();
            if from >= n {
                return Err(anyhow!("源下标越界: {from}（共 {n} 项）"));
            }
            if to > n - 1 {
                return Err(anyhow!("目标下标越界: {to}（共 {n} 项）"));
            }
            if from == to {
                return Ok(());
            }
            let v = arr.remove(from);
            arr.insert(to, v);
            Ok(())
        }
        TomlArray::Tables(arr) => {
            let n = arr.len();
            if from >= n {
                return Err(anyhow!("源下标越界: {from}（共 {n} 项）"));
            }
            if to > n - 1 {
                return Err(anyhow!("目标下标越界: {to}（共 {n} 项）"));
            }
            if from == to {
                return Ok(());
            }
            let mut items: Vec<Table> = arr.iter().cloned().collect();
            let it = items.remove(from);
            items.insert(to, it);
            arr.clear();
            for it in items {
                arr.push(it);
            }
            Ok(())
        }
    }
}

/// 递归设置 TOML 键（用递归而不是迭代，避免 `&mut Table` 自借用）
fn toml_set(table: &mut Table, path: &[String], item: toml_edit::Item) -> Result<()> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    if parents.is_empty() {
        toml_put(table, last, item);
        return Ok(());
    }
    let head = &parents[0];
    if table.get(head).is_none() {
        table.insert(head, Item::Table(Table::new()));
    }
    let child = table
        .get_mut(head)
        .ok_or_else(|| anyhow!("{head} 不存在"))?;
    let t = child
        .as_table_mut()
        .ok_or_else(|| anyhow!("{head} 不是表"))?;
    toml_set(t, &path[1..], item)
}

/// 写一个 TOML 键。
///
/// 关键点：**不能**用 `Table::insert` 替换已存在的键 —— 它会重建 Key，
/// 把键上的装饰（前导注释 `# ...`）一起抹掉。这里改成原地替换 Item，
/// 只把旧值上的行尾注释搬到新值上，其余（缩进、空行、注释）原样保留。
fn toml_put(table: &mut Table, key: &str, item: toml_edit::Item) {
    if let Some(existing) = table.get_mut(key) {
        let old_suffix = existing
            .as_value()
            .and_then(|v| v.decor().suffix().cloned());
        *existing = item;
        if let (Item::Value(nv), Some(suf)) = (&mut *existing, old_suffix) {
            nv.decor_mut().set_suffix(suf);
        }
    } else {
        table.insert(key, item);
    }
}

/// 递归删除 TOML 键
fn toml_remove(table: &mut Table, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    if parents.is_empty() {
        table.remove(last);
        return;
    }
    let head = &parents[0];
    if let Some(child) = table.get_mut(head) {
        if let Some(t) = child.as_table_mut() {
            toml_remove(t, &path[1..]);
        }
    }
}

/// JSON Value → toml_edit::Value
fn json_to_toml_value(v: &Value) -> Result<toml_edit::Value> {
    Ok(match v {
        // TOML 没有 null，用空字符串兜底
        Value::Null => toml_edit::Value::from(String::new()),
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else if let Some(f) = n.as_f64() {
                toml_edit::Value::from(f)
            } else {
                toml_edit::Value::from(n.to_string())
            }
        }
        Value::String(s) => toml_edit::Value::from(s.clone()),
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for item in a {
                arr.push(json_to_toml_value(item)?);
            }
            toml_edit::Value::Array(arr)
        }
        Value::Object(o) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, val) in o {
                t.insert(k, json_to_toml_value(val)?);
            }
            toml_edit::Value::InlineTable(t)
        }
    })
}

// ------------------------------------------------------------------- YAML

/// YAML 没有成熟的保格式库，这里做行级定点替换：
/// 扫出每个键的所在行与缩进，按路径栈匹配到目标行，再只替换值部分。
fn apply_yaml(text: &str, ops: &[EditOp]) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let trailing_newline = text.ends_with('\n');

    for op in ops {
        match op {
            EditOp::Set { path, value } => {
                if path.is_empty() {
                    continue;
                }
                let lit = scalar_literal(value, ConfigFormat::Yaml);
                set_yaml(&mut lines, path, &lit)?;
            }
            EditOp::Remove { path } => {
                remove_yaml(&mut lines, path)?;
            }
            EditOp::Insert { path, index, value } => {
                insert_yaml(&mut lines, path, *index, value)?;
            }
            EditOp::Move { path, from, to } => {
                move_yaml(&mut lines, path, *from, *to)?;
            }
        }
    }

    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

/// 一行 YAML 的结构
#[derive(Clone)]
struct YamlLine {
    /// 缩进（序列项为 `-` 所在列）
    indent: usize,
    /// 内容起始列（普通键 = indent；序列项 = `- ` 之后的列）
    content_indent: usize,
    /// 是否是序列项（`- xxx`）
    is_seq: bool,
    /// key 名；序列项是纯标量时为 None
    key: Option<String>,
    /// 值在行内的字节区间（相对整行）
    value_start: usize,
    value_end: usize,
    /// 行号
    idx: usize,
}

/// 一行 YAML 及其完整路径（路径里含序列下标）
struct YamlEntry {
    line: YamlLine,
    path: Vec<String>,
    /// 该条目代表「序列项本身」（如 `tools.0`），而不是项里的某个字段
    is_item: bool,
}

/// 解析所有「key: value」行与序列项
fn parse_yaml_lines(lines: &[String]) -> Vec<YamlLine> {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = line.len() - trimmed.len();

        // 序列项：`- xxx`
        if trimmed == "-" || trimmed.starts_with("- ") {
            let rest = &trimmed[1..];
            let skip = rest.len() - rest.trim_start().len();
            let content_off = indent + 1 + skip;
            let content = rest.trim_start();
            if content.is_empty() {
                // 光秃秃的 `-`，值在下面的块里，不支持直接改
                continue;
            }
            if let Some(colon) = find_key_colon(content) {
                let key = content[..colon]
                    .trim()
                    .trim_matches(['"', '\''])
                    .to_string();
                let (vs, ve) = yaml_value_range(content_off, colon, content);
                out.push(YamlLine {
                    indent,
                    content_indent: content_off,
                    is_seq: true,
                    key: Some(key),
                    value_start: vs,
                    value_end: ve,
                    idx,
                });
            } else {
                // 标量序列项
                let (vs, ve) = yaml_scalar_range(content_off, content);
                out.push(YamlLine {
                    indent,
                    content_indent: content_off,
                    is_seq: true,
                    key: None,
                    value_start: vs,
                    value_end: ve,
                    idx,
                });
            }
            continue;
        }

        // 普通 key: value
        let Some(colon) = find_key_colon(trimmed) else {
            continue;
        };
        let key = trimmed[..colon]
            .trim()
            .trim_matches(['"', '\''])
            .to_string();
        let (vs, ve) = yaml_value_range(indent, colon, trimmed);
        out.push(YamlLine {
            indent,
            content_indent: indent,
            is_seq: false,
            key: Some(key),
            value_start: vs,
            value_end: ve,
            idx,
        });
    }
    out
}

/// 计算 `key: value` 中值所在的字节区间。
/// `base` 是 `text` 在整行中的起始偏移，`colon` 是冒号在 `text` 中的位置。
fn yaml_value_range(base: usize, colon: usize, text: &str) -> (usize, usize) {
    let after = &text[colon + 1..];
    let lead = after.len() - after.trim_start().len();
    let value_start = base + colon + 1 + lead;
    let (val_text, _) = split_yaml_comment(after.trim_start());
    let value_end = value_start + val_text.trim_end().len();
    (value_start, value_end)
}

/// 标量（序列项里的裸值）的字节区间
fn yaml_scalar_range(base: usize, content: &str) -> (usize, usize) {
    let (val_text, _) = split_yaml_comment(content);
    (base, base + val_text.trim_end().len())
}

/// 找出 key 后的第一个冒号（跳过引号内的）
fn find_key_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b':' if !in_single
                && !in_double
                && (i + 1 >= bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t') =>
            {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

/// 拆出行尾注释：返回 (值, 注释)
fn split_yaml_comment(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single
                && !in_double
                && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') =>
            {
                return (&s[..i], &s[i..]);
            }
            _ => {}
        }
    }
    (s, "")
}

/// 给每一行算出完整路径（含序列下标）。
///
/// 路径栈：每帧记录 (缩进, 路径, 该键下已出现的序列项数)。
/// 序列项的下标由父键的计数器决定；父键被弹出时计数器自然重置。
fn resolve_yaml_paths(parsed: &[YamlLine]) -> Vec<YamlEntry> {
    struct Frame {
        indent: usize,
        path: Vec<String>,
        seq_count: usize,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut root_seq = 0usize;
    let mut out = Vec::new();

    for l in parsed {
        while let Some(f) = stack.last() {
            if f.indent >= l.indent {
                stack.pop();
            } else {
                break;
            }
        }

        if l.is_seq {
            let idx = match stack.last_mut() {
                Some(f) => {
                    let i = f.seq_count;
                    f.seq_count += 1;
                    i
                }
                None => {
                    let i = root_seq;
                    root_seq += 1;
                    i
                }
            };
            let base: Vec<String> = stack.last().map(|f| f.path.clone()).unwrap_or_default();
            let item_path: Vec<String> = base
                .iter()
                .cloned()
                .chain(std::iter::once(idx.to_string()))
                .collect();
            let line_path = match &l.key {
                Some(k) => item_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(k.clone()))
                    .collect(),
                None => item_path.clone(),
            };
            // 序列项本身也是可寻址的（`tools.0`），删除时才能整块删掉
            out.push(YamlEntry {
                line: l.clone(),
                path: item_path.clone(),
                is_item: true,
            });
            out.push(YamlEntry {
                line: l.clone(),
                path: line_path,
                is_item: false,
            });
            stack.push(Frame {
                indent: l.indent,
                path: item_path,
                seq_count: 0,
            });
        } else {
            let key = l.key.clone().unwrap_or_default();
            let base: Vec<String> = stack.last().map(|f| f.path.clone()).unwrap_or_default();
            let line_path: Vec<String> = base.iter().cloned().chain(std::iter::once(key)).collect();
            out.push(YamlEntry {
                line: l.clone(),
                path: line_path.clone(),
                is_item: false,
            });
            stack.push(Frame {
                indent: l.indent,
                path: line_path,
                seq_count: 0,
            });
        }
    }
    out
}

/// 按路径找到目标行
fn find_yaml_entry(lines: &[String], path: &[String]) -> Option<YamlEntry> {
    let parsed = parse_yaml_lines(lines);
    resolve_yaml_paths(&parsed)
        .into_iter()
        .find(|e| e.path == path)
}

/// 某个条目所属块的结束行号（含它自己的子行）
fn yaml_block_end(lines: &[String], entry: &YamlEntry) -> usize {
    let base = entry.line.indent;
    let mut end = entry.line.idx + 1;
    for (i, line) in lines.iter().enumerate().skip(entry.line.idx + 1) {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = line.len() - t.len();
        if ind <= base {
            break;
        }
        end = i + 1;
    }
    end
}

/// 新键应该用的缩进：子行的缩进，没有子行就用内容缩进
fn yaml_child_indent(lines: &[String], entry: &YamlEntry) -> usize {
    let parsed = parse_yaml_lines(lines);
    // 紧跟其后的第一行：缩进更深就是子块，缩进相同或更浅说明没有子块
    if let Some(l) = parsed.iter().find(|l| l.idx > entry.line.idx) {
        if l.indent > entry.line.indent {
            return l.content_indent;
        }
    }
    if entry.line.is_seq {
        entry.line.content_indent
    } else {
        entry.line.indent + 2
    }
}

/// 序列里每一项的起止行
struct YamlSeqItem {
    start: usize,
    end: usize,
    indent: usize,
}

/// 找出 `path` 指向的序列里的所有项
fn yaml_seq_items(lines: &[String], path: &[String]) -> Vec<YamlSeqItem> {
    let parsed = parse_yaml_lines(lines);
    let mut items: Vec<YamlSeqItem> = Vec::new();
    for e in resolve_yaml_paths(&parsed) {
        if !e.is_item {
            continue;
        }
        // 序列项路径 = 序列路径 + 下标
        if e.path.len() != path.len() + 1 {
            continue;
        }
        if &e.path[..path.len()] != path {
            continue;
        }
        let idx: usize = match e.path[path.len()].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };
        let end = yaml_block_end(lines, &e);
        while items.len() <= idx {
            items.push(YamlSeqItem {
                start: 0,
                end: 0,
                indent: 0,
            });
        }
        items[idx] = YamlSeqItem {
            start: e.line.idx,
            end,
            indent: e.line.indent,
        };
    }
    items.retain(|it| it.end > 0);
    items
}

/// 把一个值渲染成 YAML 序列项的文本（多行）
fn yaml_item_text(value: &Value, indent: usize) -> Result<String> {
    let pad = " ".repeat(indent);
    // 标量走单行
    if value.is_null() || value.is_boolean() || value.is_number() {
        return Ok(format!(
            "{pad}- {}",
            scalar_literal(value, ConfigFormat::Yaml)
        ));
    }
    if let Value::String(_) = value {
        return Ok(format!(
            "{pad}- {}",
            scalar_literal(value, ConfigFormat::Yaml)
        ));
    }
    // 复合值：交给 serde_yaml 排版，再把首行并到 `- ` 后面
    let y = serde_yaml::to_string(value).map_err(|e| anyhow!("YAML 序列化失败: {e}"))?;
    let mut out = String::new();
    let mut first = true;
    for line in y.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if first {
            out.push_str(&format!("{pad}- {}", line.trim_start()));
            first = false;
        } else {
            out.push('\n');
            out.push_str(&format!("{pad}  {}", line));
        }
    }
    if first {
        out.push_str(&format!("{pad}- "));
    }
    Ok(out)
}

/// 在 YAML 序列里插入一项
fn insert_yaml(
    lines: &mut Vec<String>,
    path: &[String],
    index: usize,
    value: &Value,
) -> Result<()> {
    let items = yaml_seq_items(lines, path);

    // 空序列：找宿主键的下一行插
    if items.is_empty() {
        let host = find_yaml_entry(lines, path)
            .ok_or_else(|| anyhow!("序列不存在: {}", path.join(".")))?;
        let indent = host.line.indent + 2;
        let text = yaml_item_text(value, indent)?;
        let at = host.line.idx + 1;
        lines.insert(at, text);
        return Ok(());
    }

    let indent = items[0].indent;
    let text = yaml_item_text(value, indent)?;

    let at = if index < items.len() {
        items[index].start
    } else {
        // 追加到最后一个项的块之后
        items[items.len() - 1].end
    };
    lines.insert(at, text);
    Ok(())
}

/// 交换 YAML 序列里的两项
fn move_yaml(lines: &mut Vec<String>, path: &[String], from: usize, to: usize) -> Result<()> {
    let items = yaml_seq_items(lines, path);
    let n = items.len();
    if n == 0 {
        return Err(anyhow!("序列不存在: {}", path.join(".")));
    }
    if from >= n {
        return Err(anyhow!("源下标越界: {from}（共 {n} 项）"));
    }
    if to >= n {
        return Err(anyhow!("目标下标越界: {to}（共 {n} 项）"));
    }
    if from == to {
        return Ok(());
    }

    // 把两项的整块文本取出来互换
    let a = &items[from];
    let b = &items[to];
    let a_text: Vec<String> = lines[a.start..a.end].to_vec();
    let b_text: Vec<String> = lines[b.start..b.end].to_vec();

    // 从后往前改：先替换行号大的那一段，避免前面的行号失效
    if a.start < b.start {
        lines.splice(b.start..b.end, a_text);
        lines.splice(a.start..a.end, b_text);
    } else {
        lines.splice(a.start..a.end, b_text);
        lines.splice(b.start..b.end, a_text);
    }
    Ok(())
}

fn set_yaml(lines: &mut Vec<String>, path: &[String], literal: &str) -> Result<()> {
    // 1) 已存在 → 就地替换值
    if let Some(e) = find_yaml_entry(lines, path) {
        if e.is_item && e.line.key.is_some() {
            return Err(anyhow!(
                "序列项是一个映射，请指定具体字段（如 {}.xxx）",
                path.join(".")
            ));
        }
        let idx = e.line.idx;
        let line = lines[idx].clone();
        let mut new_line = String::new();
        new_line.push_str(&line[..e.line.value_start]);
        if e.line.value_start == e.line.value_end {
            // 原值是空的：补空格；若后面紧跟注释，注释前也要留空格
            new_line.push(' ');
            new_line.push_str(literal);
            if line[e.line.value_start..].trim_start().starts_with('#') {
                new_line.push(' ');
            }
        } else {
            new_line.push_str(literal);
        }
        new_line.push_str(&line[e.line.value_end..]);
        lines[idx] = new_line;
        return Ok(());
    }

    // 2) 不存在 → 插到父块末尾
    let parent_path = &path[..path.len() - 1];
    let key = &path[path.len() - 1];

    // 父块也可能不存在（往没有 `sandbox:` 的配置里加 `sandbox.x`）。
    // TOML / JSON 那边都会自动建，YAML 这边也要能建：
    // 找出父链里「已存在」的最长前缀，缺失的层级逐层缩进补出来。
    let mut exist = parent_path.len();
    while exist > 0 && find_yaml_entry(lines, &parent_path[..exist]).is_none() {
        exist -= 1;
    }
    let (base_indent, insert_at) = if exist == 0 {
        (0, lines.len())
    } else {
        let parent = find_yaml_entry(lines, &parent_path[..exist])
            .ok_or_else(|| anyhow!("父路径不存在: {}", parent_path.join(".")))?;
        let ind = yaml_child_indent(lines, &parent);
        let at = yaml_block_end(lines, &parent);
        (ind, at)
    };

    // 缺失的层级 + 末段键 → 逐层缩进的块
    let mut chain: Vec<String> = parent_path[exist..].to_vec();
    chain.push(key.clone());
    // 从深到浅插（insert 会把后面的往后推，反着插正好还原顺序）
    for i in (0..chain.len()).rev() {
        let mut new_line = " ".repeat(base_indent + i * 2);
        new_line.push_str(&chain[i]);
        new_line.push(':');
        if i + 1 == chain.len() {
            new_line.push(' ');
            new_line.push_str(literal);
        }
        lines.insert(insert_at, new_line);
    }
    Ok(())
}

fn remove_yaml(lines: &mut Vec<String>, path: &[String]) -> Result<()> {
    let Some(e) = find_yaml_entry(lines, path) else {
        return Ok(());
    };
    let start = e.line.idx;
    let end = yaml_block_end(lines, &e);
    lines.drain(start..end);
    Ok(())
}

// ------------------------------------------------------------------ 保存

/// 保存结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveResult {
    pub path: String,
    pub bytes: usize,
    pub backup: Option<String>,
}

/// 原子写入：先写同目录临时文件再 rename，并留一份备份
pub fn save(path: &Path, new_text: &str, backup: bool) -> Result<SaveResult> {
    let path = path.to_path_buf();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut backup_path = None;
    if backup && path.exists() {
        let old = std::fs::read_to_string(&path)?;
        if old != new_text {
            let bak = backup_dir_for(&path);
            std::fs::create_dir_all(&bak)?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("config");
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let bak_file = bak.join(format!("{name}.{stamp}.bak"));
            std::fs::write(&bak_file, old)?;
            backup_path = Some(bak_file.display().to_string());
        }
    }

    let tmp: PathBuf = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("cfg")
    ));
    std::fs::write(&tmp, new_text)
        .with_context(|| format!("写入临时文件失败: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("替换原文件失败: {}", path.display()))?;

    Ok(SaveResult {
        path: path.display().to_string(),
        bytes: new_text.len(),
        backup: backup_path,
    })
}

/// 备份目录：同级的 `.cfg-backups/`
fn backup_dir_for(path: &Path) -> PathBuf {
    path.parent().unwrap_or(Path::new(".")).join(".cfg-backups")
}

/// 生成两版文本的差异摘要（用于保存前确认）
pub fn diff_summary(old: &str, new: &str) -> Vec<String> {
    let mut out = Vec::new();
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let max = old_lines.len().max(new_lines.len());
    let mut shown = 0;
    for i in 0..max {
        let o = old_lines.get(i);
        let n = new_lines.get(i);
        if o != n {
            let (o, n) = match (o, n) {
                (Some(a), Some(b)) => (a.to_string(), b.to_string()),
                (Some(a), None) => (a.to_string(), "（删除）".into()),
                (None, Some(b)) => ("（新增）".into(), b.to_string()),
                (None, None) => continue,
            };
            out.push(format!("第{}行\n  - {}\n  + {}", i + 1, o.trim(), n.trim()));
            shown += 1;
            if shown >= 40 {
                out.push("…（差异过多，已截断）".into());
                break;
            }
        }
    }
    out
}
