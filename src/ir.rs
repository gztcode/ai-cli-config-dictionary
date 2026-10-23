//! 中间表示（IR）。
//!
//! 这是整个项目的关键契约：GUI 只消费 IR，不直接碰 JSON Schema。
//! IR 必须同时满足两件事——
//!   1. 渲染需要：含义、文档链接、类型、枚举、约束
//!   2. 校验需要：足够的信息来判定「格式对不对、key 错没错」

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 值的类型。`Enum` 表示取值被固定枚举约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Number,
    Integer,
    Boolean,
    Object,
    Array,
    Enum,
    Null,
    Any,
}

impl ValueType {
    pub fn label(&self) -> &'static str {
        match self {
            ValueType::String => "string",
            ValueType::Number => "number",
            ValueType::Integer => "integer",
            ValueType::Boolean => "boolean",
            ValueType::Object => "object",
            ValueType::Array => "array",
            ValueType::Enum => "enum",
            ValueType::Null => "null",
            ValueType::Any => "any",
        }
    }
}

/// 枚举成员。description 来自 `oneOf` 分支的 description 或 `x-enum-descriptions`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 标量约束，直接用于校验。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Constraints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
}

/// 该节点在树中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// 叶子节点，用户直接填值
    Leaf,
    /// 对象节点，可展开出子键
    Object,
    /// 数组节点
    Array,
}

/// 一个配置键（树形）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrKey {
    /// 点路径，如 `permissions.allow`
    pub path: String,
    /// 末段名称
    pub name: String,
    pub kind: KeyKind,
    pub value_type: ValueType,
    /// 是否允许 null
    pub nullable: bool,
    /// 官方描述（英文原文）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 官方文档链接（从描述中抽取，已去重）
    pub doc_urls: Vec<String>,
    /// 枚举取值
    pub enum_values: Vec<EnumValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    pub examples: Vec<Value>,
    pub deprecated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_note: Option<String>,
    /// 是否必填（父对象 required 列表中）
    pub required: bool,
    pub constraints: Constraints,
    /// 该对象允许任意子键 —— 这正是「手动输入自定义 key」的合法区域
    pub free_form: bool,
    /// 子键
    pub children: Vec<IrKey>,
    /// 多语言覆盖层：locale -> 描述
    pub translations: BTreeMap<String, String>,
}

impl IrKey {
    /// 递归统计叶子节点数
    pub fn leaf_count(&self) -> usize {
        if self.children.is_empty() {
            1
        } else {
            self.children.iter().map(|c| c.leaf_count()).sum()
        }
    }

    /// 递归收集全部文档链接
    pub fn collect_doc_urls(&self, out: &mut Vec<String>) {
        for u in &self.doc_urls {
            if !out.contains(u) {
                out.push(u.clone());
            }
        }
        for c in &self.children {
            c.collect_doc_urls(out);
        }
    }
}

/// schema 来源元信息，用于溯源与缓存失效判断。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSource {
    pub url: String,
    pub origin: String,
    pub sha256: String,
    pub fetched_at: String,
    pub bytes: usize,
}

/// IR 统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IrStats {
    pub total_keys: usize,
    pub leaf_keys: usize,
    pub documented: usize,
    pub with_doc_url: usize,
    pub with_enum: usize,
    pub with_default: usize,
    pub deprecated: usize,
    pub translated: usize,
    /// 叶子键中有官方说明的比例 —— 这才是 GUI 用户实际能看到的覆盖率
    pub leaf_documented: usize,
    pub leaf_with_doc_url: usize,
    pub leaf_with_enum: usize,
    /// 叶子键总数（与 leaf_keys 相同，便于前端直接算百分比）
    pub leaf_total: usize,
}

/// 一家工具的完整 IR。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ir {
    pub tool: String,
    pub display_name: String,
    pub docs_url: String,
    pub source: SchemaSource,
    pub generated_at: String,
    /// 根节点的子键集合
    pub keys: Vec<IrKey>,
    pub stats: IrStats,
}

impl Ir {
    /// 深度优先遍历全部节点
    pub fn walk<'a>(&'a self, keys: &'a [IrKey], prefix: &str, f: &mut dyn FnMut(&str, &IrKey)) {
        for k in keys {
            let path = if prefix.is_empty() {
                k.path.clone()
            } else {
                format!("{prefix}.{}", k.name)
            };
            f(&path, k);
            self.walk(&k.children, &path, f);
        }
    }
}
