//! 注册表：描述一家工具的元信息与 schema 来源。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一家工具的注册信息。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tool {
    pub id: String,
    pub display_name: String,
    /// schema 来源分类：schemastore | official | official-repo | 空=无
    #[serde(default)]
    pub schema_origin: String,
    /// 机器可读 schema 地址，空表示暂无
    #[serde(default)]
    pub schema_url: String,
    /// 内置的本地 schema 文件名（在 `schemas/` 目录下）。
    ///
    /// 有些工具没有发布机器可读的 schema，但官方文档里有结构化的字段表，
    /// 我们据此派生出 schema 并随仓库分发，避免「编造键位」。
    #[serde(default)]
    pub schema_file: String,
    /// 「文档补充层」文件名（在 `schemas/` 目录下）。
    ///
    /// 格式：`{ "键路径": { "description": "...", "doc_url": "..." } }`。
    /// 官方 schema 照旧联网自动更新，文档里的说明与章节锚点由这一层叠加上去 ——
    /// 官方 schema 常常只给结构不给文字说明。
    #[serde(default)]
    pub doc_supplement: String,
    /// 官方文档首页
    #[serde(default)]
    pub docs_url: String,
    /// 配置文件格式：json | jsonc | toml | yaml
    pub config_format: String,
    /// 全局配置路径，支持 `~`
    #[serde(default)]
    pub global_config: Vec<String>,
    /// 项目级配置路径（相对当前项目目录）
    #[serde(default)]
    pub project_config: Vec<String>,
}

impl Tool {
    pub fn has_schema(&self) -> bool {
        !self.schema_url.trim().is_empty() || !self.schema_file.trim().is_empty()
    }
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct Registry {
    #[serde(default, rename = "tool")]
    pub tools: Vec<Tool>,
}

impl Registry {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取注册表失败: {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("解析注册表失败")
    }

    pub fn get(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }
}

/// 展开 `~` 并返回绝对路径。
pub fn expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// 当前时间（UTC），格式 `YYYY-MM-DDTHH:MM:SSZ`。避免额外引入 chrono。
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 1970-01-01 起的天数转日期（民用格里高利历算法）
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// SHA-256 十六进制摘要。
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
