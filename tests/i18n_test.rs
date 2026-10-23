//! 内置中文词条的质量测试。
//!
//! `src/i18n/zh-CN.json` 是机器翻译出来的离线词条，最容易出的问题是：
//! 覆盖不全、占位符被吃掉、译文其实没翻、术语表形同虚设。
//! 这里把这几件事都钉住 —— 翻译脚本每次改动都要过这一关。

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// 读内置词条
fn builtin() -> BTreeMap<String, String> {
    let txt = include_str!("../src/i18n/zh-CN.json");
    let m: serde_json::Map<String, Value> =
        serde_json::from_str(txt).expect("内置词条不是合法 JSON");
    let mut out = BTreeMap::new();
    for (k, v) in m {
        if k.starts_with("__") {
            continue; // __meta__ 是元信息
        }
        let s = v.as_str().expect("词条值必须是字符串").to_string();
        out.insert(k, s);
    }
    out
}

/// 从 IR 里取出所有「带说明的键」，返回 tool → {path → description}
fn ir_descriptions() -> BTreeMap<String, BTreeMap<String, String>> {
    let mut all = BTreeMap::new();
    for entry in glob_ir("out/ir") {
        let txt = std::fs::read_to_string(&entry).expect("读不到 IR 文件");
        let d: Value = serde_json::from_str(&txt).expect("IR 不是合法 JSON");
        let tool = d["tool"].as_str().expect("IR 缺 tool").to_string();
        let mut map = BTreeMap::new();
        collect(&d["keys"], &mut map);
        all.insert(tool, map);
    }
    all
}

/// 极简 glob：只支持 `dir/*.json`
fn glob_ir(dir: &str) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("json") {
                v.push(p.to_string_lossy().to_string());
            }
        }
    }
    v.sort();
    v
}

fn collect(keys: &Value, out: &mut BTreeMap<String, String>) {
    let Some(arr) = keys.as_array() else { return };
    for k in arr {
        let path = k["path"].as_str().unwrap_or_default().to_string();
        let desc = k["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        if !desc.is_empty() {
            out.insert(path, desc);
        }
        collect(&k["children"], out);
    }
}

/// 必须原样保留的片段（和 tools/translate.py 的闸门同一套规则）
fn keepset(text: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let b: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        // 反引号内容
        if c == '`' {
            let mut j = i + 1;
            while j < b.len() && b[j] != '`' {
                j += 1;
            }
            if j < b.len() {
                out.insert(b[i..=j].iter().collect::<String>());
                i = j + 1;
                continue;
            }
        }
        // 尖括号 / 花括号占位符
        if c == '<' || c == '{' {
            let close = if c == '<' { '>' } else { '}' };
            let mut j = i + 1;
            while j < b.len() && b[j] != close {
                j += 1;
            }
            if j < b.len() && j > i + 1 {
                out.insert(b[i..=j].iter().collect::<String>());
                i = j + 1;
                continue;
            }
        }
        // 全大写 token（≥4 字符）
        if c.is_ascii_uppercase() {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_uppercase() || b[j].is_ascii_digit() || b[j] == '_')
            {
                j += 1;
            }
            if j - i >= 4 {
                let word: String = b[i..j].iter().collect();
                let prev = if i > 0 { b[i - 1] } else { ' ' };
                let next = if j < b.len() { b[j] } else { ' ' };
                // 文件名（CLAUDE.md）：点号后面必须紧跟小写字母，
                // 否则句末的句点会被误判成扩展名。
                let file_ext = next == '.' && j + 1 < b.len() && b[j + 1].is_ascii_lowercase();
                // 与 tools/translate.py 的闸门同一套判定：
                // 只有「像标识符」的大写词才要求原样保留 ——
                // 含下划线/数字、前带 $、文件名、真缩写。
                // DISCOURAGED / PAID / IGNORED 这类**用大写强调的普通词**
                // 应当被本地化，否则闸门会把它们永远判不合格。
                let ident = word.contains('_')
                    || word.chars().any(|c| c.is_ascii_digit())
                    || prev == '$'
                    || file_ext
                    || is_preserve_exact(&word)
                    || is_acronym(&word);
                if ident {
                    out.insert(word);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// shell / git 的标准名，即使也是普通英文词也要保留
fn is_preserve_exact(w: &str) -> bool {
    matches!(
        w,
        "PATH" | "HEAD" | "HOME" | "USER" | "SHELL" | "PWD" | "ORIG_HEAD" | "HTTPS" | "HTTP"
    )
}

/// 真正的缩写 / 术语
fn is_acronym(w: &str) -> bool {
    const LIST: &[&str] = &[
        "JSON",
        "POST",
        "HTTP",
        "HTTPS",
        "OTLP",
        "VRAM",
        "CLI",
        "API",
        "URL",
        "MCP",
        "LLM",
        "TLS",
        "SSH",
        "TUI",
        "IDE",
        "ISO",
        "SHA",
        "ANSI",
        "CORS",
        "UUID",
        "SOCKS",
        "MITM",
        "HKCU",
        "HKLM",
        "YAML",
        "TOML",
        "CPU",
        "GPU",
        "RAM",
        "SSE",
        "WS",
        "OAUTH",
        "JWT",
        "CSV",
        "XML",
        "HTML",
        "CSS",
        "SDK",
        "UTC",
        "CHANGELOG",
        "YOLO",
        "PNG",
        "UTF",
        "IPC",
        "PID",
        "TTL",
        "GREP",
    ];
    LIST.contains(&w)
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 1. 覆盖完整：IR 里每条带说明的键都必须有中文词条
#[test]
fn 覆盖层与_ir_完全对齐() {
    let ir = ir_descriptions();
    if ir.is_empty() {
        // 没有 IR 产物时跳过（首次 clone 仓库会这样）
        eprintln!("跳过：out/ir 下没有 IR 文件");
        return;
    }
    let cov = builtin();
    let mut missing = Vec::new();
    for (tool, map) in &ir {
        for path in map.keys() {
            let k = format!("{tool}:{path}");
            if !cov.contains_key(&k) {
                missing.push(k);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "有 {} 条键没有中文词条，例如: {:?}",
        missing.len(),
        &missing[..missing.len().min(10)]
    );
}

/// 2. 词条 key 格式统一为 `{tool_id}:{key_path}`
#[test]
fn 词条键格式正确() {
    let cov = builtin();
    assert!(!cov.is_empty(), "内置词条是空的");
    for k in cov.keys() {
        assert!(k.contains(':'), "词条键缺少 `tool:path` 结构: {k}");
        let (tool, path) = k.split_once(':').unwrap();
        assert!(!tool.is_empty(), "词条键缺工具 id: {k}");
        assert!(!path.is_empty(), "词条键缺 key 路径: {k}");
        assert!(
            !path.starts_with('.') && !path.ends_with('.'),
            "key 路径首尾有多余的点: {k}"
        );
    }
}

/// 3. 占位符完整性：译文必须保留所有标识符片段（反引号、占位符、环境变量名）
#[test]
fn 译文不丢占位符与标识符() {
    let cov = builtin();
    let mut bad = Vec::new();
    for (k, zh) in &cov {
        // 从英文原文反推必须保留的片段：没有原文就跳过这条断言
        let Some(en) = en_for(k) else { continue };
        for frag in keepset(&en) {
            if !zh.contains(&frag) {
                bad.push(format!("{k} 丢失 {frag}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} 处占位符/标识符丢失，例如: {:?}",
        bad.len(),
        &bad[..bad.len().min(10)]
    );
}

/// 4. 译文真的被翻译了：不能还是英文原文
#[test]
fn 译文不是英文原文() {
    let cov = builtin();
    let same: Vec<&String> = cov
        .iter()
        .filter(|(k, zh)| {
            let en = en_for(k).unwrap_or_default();
            !en.is_empty() && en.trim() == zh.trim()
        })
        .map(|(k, _)| k)
        .collect();
    assert!(
        same.is_empty(),
        "{} 条词条没有翻译（与英文原文一致），例如: {:?}",
        same.len(),
        &same[..same.len().min(10)]
    );
}

/// 5. 译文确实是中文
#[test]
fn 译文含中文() {
    let cov = builtin();
    let no_cjk: Vec<&String> = cov
        .iter()
        .filter(|(_, zh)| !has_cjk(zh))
        .map(|(k, _)| k)
        .collect();
    assert!(
        no_cjk.is_empty(),
        "{} 条词条里没有中文字符，例如: {:?}",
        no_cjk.len(),
        &no_cjk[..no_cjk.len().min(10)]
    );
}

/// 6. 术语一致性：术语表里的词在中文译文里不应再以英文形式出现
#[test]
fn 术语表里的词已被本地化() {
    const TERMS: &[(&str, &str)] = &[
        ("model", "模型"),
        ("permission", "权限"),
        ("telemetry", "遥测"),
        ("sandbox", "沙箱"),
        ("plugin", "插件"),
        ("workflow", "工作流"),
        ("checkpoint", "检查点"),
    ];
    let cov = builtin();
    for (k, zh) in &cov {
        for (en, zh_word) in TERMS {
            // 只查独立出现的英文术语，且要求译文里没有对应的中文词。
            // 「独立出现」= 前后都不是字母数字、- _ /：
            // `--permission-mode` 是命令行标志、`/voice model` 是斜杠命令，都不算。
            if zh.contains(zh_word) {
                continue;
            }
            let hit = standalone_english(zh, en);
            assert!(
                !hit,
                "{k} 的译文里术语 `{en}` 没有本地化（应为「{zh_word}」）: {zh}"
            );
        }
    }
}

/// 从 IR 里按 `tool:path` 取英文原文
fn en_for(key: &str) -> Option<String> {
    let (tool, path) = key.split_once(':')?;
    let ir = ir_descriptions();
    ir.get(tool)?.get(path).cloned()
}

/// 文本里是否「独立出现」某个英文词（前后都不是字母数字 / - _ /）
fn standalone_english(text: &str, word: &str) -> bool {
    let b: Vec<char> = text.chars().collect();
    let w: Vec<char> = word.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '-' || c == '_' || c == '/';
    if w.is_empty() {
        return false;
    }
    for i in 0..b.len() {
        if i + w.len() > b.len() {
            break;
        }
        if b[i..i + w.len()]
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case(word)
        {
            let before_ok = i == 0 || !is_word(b[i - 1]);
            let after_idx = i + w.len();
            let after_ok = after_idx >= b.len() || !is_word(b[after_idx]);
            // 引号 / 反引号包起来的是字面量（字段名 / 枚举值 / 键路径），不是术语
            let before = if i > 0 { b[i - 1] } else { ' ' };
            let after = if after_idx < b.len() {
                b[after_idx]
            } else {
                ' '
            };
            let quoted = matches!(before, '\'' | '"' | '`') && (after == before || before == '`');
            // 点分键路径里的分量（`cron.model`、`permissions.allow`）不是术语
            let after_dot_is_word =
                after == '.' && after_idx + 1 < b.len() && b[after_idx + 1].is_ascii_lowercase();
            let path_component = before == '.' || after_dot_is_word;
            // 中文顿号分隔的字段名列表（`可用字段：model、context_detail`）不是术语
            let in_list = before == '、' || after == '、' || before == '，' || after == '，';
            // 反引号里的内容是字面量（`hermes model`、`model`）→ 不是术语
            let in_code = in_backticks(&b, i);
            // 方括号 / 花括号里的路径引用（`[model.<id>]`）不是术语
            let in_path = in_brackets(&b, i);
            if before_ok
                && after_ok
                && !quoted
                && !path_component
                && !in_list
                && !in_code
                && !in_path
                && !in_slash_command(&b, i)
            {
                return true;
            }
        }
    }
    false
}

/// 命中位置是否在反引号里（行内代码 / 命令名）
fn in_backticks(b: &[char], at: usize) -> bool {
    let ticks = b[..at].iter().filter(|c| **c == '`').count();
    ticks % 2 == 1
}

/// 命中位置是否在方括号 / 花括号里（路径或字面量引用）
fn in_brackets(b: &[char], at: usize) -> bool {
    let square = b[..at].iter().filter(|c| **c == '[').count() as i32
        - b[..at].iter().filter(|c| **c == ']').count() as i32;
    let curly = b[..at].iter().filter(|c| **c == '{').count() as i32
        - b[..at].iter().filter(|c| **c == '}').count() as i32;
    square > 0 || curly > 0
}

/// 命中位置是否属于一个斜杠命令（`/voice model` 里的 model 是命令名，不是术语）
fn in_slash_command(b: &[char], at: usize) -> bool {
    // 往回找**最近**的 '/'，中间只能有命令名允许的字符
    let mut j = at;
    while j > 0 {
        j -= 1;
        let c = b[j];
        if c == '/' {
            // '/' 与命中位置之间不能再有别的 '/'（否则那是另一个命令）
            return !b[j + 1..at].contains(&'/');
        }
        if !(c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' || c == ',' || c == '，') {
            return false;
        }
    }
    false
}
