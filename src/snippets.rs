//! 配置片段库。
//!
//! 一条「片段」是一组可以直接写进配置文件的键值 —— 常见的调优、
//! 安全加固、模型切换之类的手写片段。应用它时走和编辑器一样的
//! [`crate::edit`] 通道，所以**原格式（缩进 / 注释 / 键顺序）照样保留**。
//!
//! 片段库存在用户的配置目录里（`snippets.json`），用户可以增删改；
//! 内置的那几条是「官方推荐」的开头，用户随便改。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::edit::EditOp;

/// 片段里的一项：要写进哪个键、写成什么
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetItem {
    /// 点分路径，例如 `permissions.defaultMode`
    pub path: String,
    /// 目标值；`null` 表示「删除这个键」
    pub value: Value,
}

/// 一条配置片段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    /// 稳定 id（用户自建时由前端生成）
    pub id: String,
    /// 片段名（列表里显示）
    pub name: String,
    /// 适用的工具 id；空 = 通用
    #[serde(default)]
    pub tool: String,
    /// 一句话说明这个片段干什么
    #[serde(default)]
    pub description: String,
    /// 要写进配置的键值
    pub items: Vec<SnippetItem>,
    /// 内置片段（用户可以改内容，但条目本身留着）
    #[serde(default)]
    pub builtin: bool,
}

impl Snippet {
    /// 片段对应的编辑操作
    pub fn ops(&self) -> Vec<EditOp> {
        self.items
            .iter()
            .map(|it| {
                let segs: Vec<String> = it
                    .path
                    .split('.')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if it.value.is_null() {
                    EditOp::Remove { path: segs }
                } else {
                    EditOp::Set {
                        path: segs,
                        value: it.value.clone(),
                    }
                }
            })
            .collect()
    }
}

/// 内置片段库
///
/// 每一条都对应各工具官方文档里明确写过的配置项 —— 不编造键位。
pub fn builtin_snippets() -> Vec<Snippet> {
    vec![
        Snippet {
            id: "claude-code-perm-allow-all".into(),
            name: "Claude Code：放开所有工具权限".into(),
            tool: "claude-code".into(),
            description: "把权限模式设成 bypassPermissions。仅建议在隔离环境里用。".into(),
            items: vec![
                SnippetItem {
                    path: "permissions.defaultMode".into(),
                    value: json!("bypassPermissions"),
                },
                SnippetItem {
                    path: "permissions.allow".into(),
                    value: json!(["Bash", "Read", "Edit", "Write"]),
                },
            ],
            builtin: true,
        },
        Snippet {
            id: "claude-code-sandbox".into(),
            name: "Claude Code：开沙箱".into(),
            tool: "claude-code".into(),
            description: "把命令放进沙箱里跑，并允许沙箱内直接放行 Bash。".into(),
            items: vec![
                SnippetItem {
                    path: "sandbox.enabled".into(),
                    value: json!(true),
                },
                SnippetItem {
                    path: "sandbox.autoAllowBashIfSandboxed".into(),
                    value: json!(true),
                },
            ],
            builtin: true,
        },
        Snippet {
            id: "codex-reasoning-high".into(),
            name: "Codex：把推理强度调到 high".into(),
            tool: "codex".into(),
            description: "model_reasoning_effort = \"high\"，适合复杂任务。".into(),
            items: vec![SnippetItem {
                path: "model_reasoning_effort".into(),
                value: json!("high"),
            }],
            builtin: true,
        },
        Snippet {
            id: "gemini-ripgrep".into(),
            name: "Gemini CLI：放开受信工具 + 用 ripgrep".into(),
            tool: "gemini-cli".into(),
            description: "把 Bash / ReadWrite / Edit 加进免确认清单，并用 ripgrep 代替内置 grep。"
                .into(),
            items: vec![
                SnippetItem {
                    path: "tools.allowed".into(),
                    value: json!(["Bash", "ReadWrite", "Edit"]),
                },
                SnippetItem {
                    path: "tools.useRipgrep".into(),
                    value: json!(true),
                },
            ],
            builtin: true,
        },
        Snippet {
            id: "opencode-perm-edit".into(),
            name: "OpenCode：编辑与命令一律允许".into(),
            tool: "opencode".into(),
            description: "permission 里把 edit / bash / webfetch 设为 allow。".into(),
            items: vec![
                SnippetItem {
                    path: "permission.edit".into(),
                    value: json!("allow"),
                },
                SnippetItem {
                    path: "permission.bash".into(),
                    value: json!("allow"),
                },
                SnippetItem {
                    path: "permission.webfetch".into(),
                    value: json!("allow"),
                },
            ],
            builtin: true,
        },
        Snippet {
            id: "hermes-memories".into(),
            name: "Hermes：开记忆 + 关写记忆审批".into(),
            tool: "hermes".into(),
            description: "打开 memory.memory_enabled，并把 write_approval 关掉（少一次确认）。"
                .into(),
            items: vec![
                SnippetItem {
                    path: "memory.memory_enabled".into(),
                    value: json!(true),
                },
                SnippetItem {
                    path: "memory.write_approval".into(),
                    value: json!(false),
                },
            ],
            builtin: true,
        },
    ]
}

/// 片段库文件
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnippetStore {
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

/// 读片段库
///
/// 磁盘上的文件是权威的（用户改过什么就是什么）；
/// 但如果以后版本新增了内置片段，它会自动补进来 —— 磁盘上没有的不丢。
pub fn load(path: &std::path::Path) -> SnippetStore {
    let mut out: Vec<Snippet> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(store) = serde_json::from_str::<SnippetStore>(&text) {
            out = store.snippets;
        }
    }
    if out.is_empty() {
        out = builtin_snippets();
    } else {
        // 补上磁盘上没有的内置片段
        for b in builtin_snippets() {
            if !out.iter().any(|s| s.id == b.id) {
                out.push(b);
            }
        }
    }
    SnippetStore { snippets: out }
}

/// 写片段库（整份覆盖）
pub fn save(path: &std::path::Path, snippets: &[Snippet]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&SnippetStore {
        snippets: snippets.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}
