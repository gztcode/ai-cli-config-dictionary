//! 密钥识别与脱敏。
//!
//! 原则：**脱敏发生在服务端**——前端拿到的配置树里，密钥值已经是掩码，
//! 明文只在用户显式点「显示」时通过 `reveal_secret` 单次取回。
//!
//! 写回安全阀：掩码值（含有 `****`）不会被当成真值写进文件，
//! 避免「用户没改这个字段，却把脱敏后的串写进了配置文件」这种事故。

use serde_json::Value;

/// 强信号：整段命中就一定是密钥
const STRONG_TOKENS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "secrets",
    "credential",
    "credentials",
    "cred",
];

/// 复合信号：必须整组出现才算
/// （`api_key` 命中，但 `max_token` / `maxTokens` 不命中）
const COMPOUND_TOKENS: &[&str] = &[
    "api_key",
    "api_keys",
    "access_key",
    "access_keys",
    "secret_key",
    "auth_token",
    "bearer_token",
    "refresh_token",
    "access_token",
    "id_token",
    "session_key",
    "signing_key",
    "encryption_key",
    "private_key",
    "client_secret",
];

/// 弱信号：只有整段就是这个词才算
/// （`token` 是密钥，但 `maxToken` 是计数；`auth` 是密钥，但 `authorization` 不是）
const EXACT_TOKENS: &[&str] = &["token", "tokens", "auth", "bearer"];

/// 掩码里的固定标记：既是给人看的，也是给安全闸门识别的
pub const MASK_MARK: &str = "****";

/// 判定一个配置路径是否属于密钥字段。
///
/// 路径段先归一化（小写 + 驼峰转下划线），再分级匹配：
/// - `ANTHROPIC_API_KEY` → `anthropic_api_key` → 复合信号 `api_key` ✓
/// - `awsSecretAccessKey` → `aws_secret_access_key` → 复合信号 `access_key` ✓
/// - `maxTokens` → `max_tokens` → 不命中 ✓（AI 配置里 token 也指计数）
/// - `tokenizer` → 不命中 ✓
pub fn is_secret_path(path: &[String]) -> bool {
    path.iter().any(|seg| {
        let norm = normalize(seg);
        if norm.is_empty() {
            return false;
        }
        let segs: Vec<&str> = norm.split('_').filter(|s| !s.is_empty()).collect();

        // 1) 强信号：整段命中
        if segs.iter().any(|s| STRONG_TOKENS.contains(s)) {
            return true;
        }
        // 2) 复合信号：连续分段命中
        for tok in COMPOUND_TOKENS {
            if seg_contains(&segs, tok) {
                return true;
            }
        }
        // 3) 弱信号：整段就是这个词
        if segs.len() == 1 && EXACT_TOKENS.contains(&segs[0]) {
            return true;
        }
        // 4) 无分隔符的复合写法：apikey / accesskey
        let flat: String = segs.concat();
        if flat.contains("apikey")
            || flat.contains("accesskey")
            || flat.contains("secretkey")
            || flat.contains("privatekey")
            || flat.contains("clientsecret")
        {
            return true;
        }
        false
    })
}

/// 关键词是否出现在分段序列里（词边界匹配）
///
/// `tok` 含 `_` 时按连续分段匹配：`api_key` 能命中 `anthropic_api_key`。
fn seg_contains(segs: &[&str], tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('_').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    if parts.len() == 1 {
        return segs.iter().any(|s| *s == parts[0]);
    }
    if segs.len() < parts.len() {
        return false;
    }
    segs.windows(parts.len()).any(|w| w == parts.as_slice())
}

/// 归一化：转小写，插入分隔符
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            // 驼峰边界：前一个是小写/数字、后一个也是小写 → 视为词边界
            let prev_lower =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if prev_lower && next_lower {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else if c == '-' || c == ' ' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

/// 生成掩码：保留前 4 后 4，中间用 `****`。
///
/// 太短的值（≤ 8 字符）全部打码，避免泄露过多。
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return MASK_MARK.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}{MASK_MARK}{tail}")
}

/// 判断一个字符串是否已经是掩码（写回安全阀用）
pub fn is_masked(value: &str) -> bool {
    value.contains(MASK_MARK)
}

/// 对配置树做脱敏。
///
/// 返回 (脱敏后的树, 密钥字段的路径列表)。
/// 密钥路径用点分形式，数组下标用数字，便于前端定位。
pub fn redact(value: &Value, prefix: &[String]) -> (Value, Vec<String>) {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            let mut secrets = Vec::new();
            for (k, v) in map {
                let mut p = prefix.to_vec();
                p.push(k.clone());
                if is_secret_path(&p) {
                    if let Value::String(s) = v {
                        out.insert(k.clone(), Value::String(mask(s)));
                        secrets.push(p.join("."));
                        continue;
                    }
                }
                let (rv, rs) = redact(v, &p);
                out.insert(k.clone(), rv);
                secrets.extend(rs);
            }
            (Value::Object(out), secrets)
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            let mut secrets = Vec::new();
            for (i, v) in items.iter().enumerate() {
                let mut p = prefix.to_vec();
                p.push(i.to_string());
                if is_secret_path(&p) {
                    if let Value::String(s) = v {
                        out.push(Value::String(mask(s)));
                        secrets.push(p.join("."));
                        continue;
                    }
                }
                let (rv, rs) = redact(v, &p);
                out.push(rv);
                secrets.extend(rs);
            }
            (Value::Array(out), secrets)
        }
        other => (other.clone(), Vec::new()),
    }
}

/// 按点分路径从配置树里取原始值（reveal_secret 用，不过脱敏）
pub fn get_by_dotted(root: &Value, dotted: &str) -> Option<Value> {
    let mut cur = root;
    for seg in dotted.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}
