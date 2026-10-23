//! 密钥识别与脱敏的测试。
//!
//! 命题：
//! 1. 该被认成密钥的字段一定被认出来
//! 2. 不该被认成密钥的字段不能误伤（AI 配置里 token 也指计数）
//! 3. 脱敏后明文不出现在返回结构里
//! 4. 掩码能被可靠识别（写回安全阀靠它）

use cfg_explorer::secret;
use serde_json::json;

fn is_secret(segments: &[&str]) -> bool {
    secret::is_secret_path(&segments.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

#[test]
fn 该认出来的密钥字段都认得出来() {
    let cases: &[&[&str]] = &[
        &["env", "ANTHROPIC_API_KEY"],
        &["env", "ANTHROPIC_AUTH_TOKEN"],
        &["env", "ANTHROPIC_AWS_ACCESS_KEY_ID"],
        &["apiKey"],
        &["APIKey"],
        &["api_key"],
        &["awsSecretAccessKey"],
        &["clientSecret"],
        &["private_key"],
        &["password"],
        &["passwd"],
        &["pwd"],
        &["credential"],
        &["credentials"],
        &["bearerToken"],
        &["refresh_token"],
        &["authToken"],
        &["token"],
        &["auth"],
    ];
    for c in cases {
        assert!(is_secret(c), "应当判为密钥: {c:?}");
    }
}

#[test]
fn 普通字段不会被误判成密钥() {
    let cases: &[&[&str]] = &[
        &["model"],
        &["maxTokens"],
        &["max_token"],
        &["outputTokens"],
        &["tokenizer"],
        &["permissions", "allow"],
        &["authorization"],
        &["sortedKeys"],
        &["reasoning"],
        &["env", "FOO"],
        &["includeCoAuthoredBy"],
        &["cleanupPeriodDays"],
        &["statusLine"],
    ];
    for c in cases {
        assert!(!is_secret(c), "不该判为密钥: {c:?}");
    }
}

#[test]
fn 脱敏后明文不出现在树里() {
    let v = json!({
        "model": "opus",
        "env": { "ANTHROPIC_API_KEY": "sk-ant-supersecret-value" },
        "nested": { "deep": { "password": "hunter2" } }
    });
    let (redacted, secrets) = secret::redact(&v, &[]);
    let s = serde_json::to_string(&redacted).unwrap();
    assert!(!s.contains("supersecret"), "明文泄漏了: {s}");
    assert!(!s.contains("hunter2"), "明文泄漏了: {s}");
    // 掩码保留了首尾，便于辨认是哪家的 key
    assert!(s.contains("sk-a"), "应保留前缀: {s}");
    // 密钥路径被记录下来
    assert!(
        secrets.contains(&"env.ANTHROPIC_API_KEY".to_string()),
        "{secrets:?}"
    );
    assert!(
        secrets.contains(&"nested.deep.password".to_string()),
        "{secrets:?}"
    );
}

#[test]
fn 脱敏不改动非密钥字段() {
    let v = json!({ "model": "opus", "maxTokens": 4096, "enabled": true });
    let (redacted, secrets) = secret::redact(&v, &[]);
    assert_eq!(redacted, v);
    assert!(secrets.is_empty());
}

#[test]
fn 掩码能被可靠识别() {
    assert!(secret::is_masked(&secret::mask("sk-ant-1234567890")));
    assert!(secret::is_masked("sk-a****7890"));
    assert!(!secret::is_masked("sk-ant-1234567890"));
    assert!(!secret::is_masked(""));
}

#[test]
fn 短值整体打码不泄露() {
    // 长度不足 9 的值全部打码，避免泄露过多
    assert_eq!(secret::mask("short"), "****");
    assert_eq!(secret::mask("12345678"), "****");
    assert_eq!(secret::mask("123456789"), "1234****6789");
}

#[test]
fn 按点分路径能取到原值() {
    let v = json!({ "env": { "ANTHROPIC_API_KEY": "sk-real" }, "arr": ["a", "b"] });
    assert_eq!(
        secret::get_by_dotted(&v, "env.ANTHROPIC_API_KEY"),
        Some(json!("sk-real"))
    );
    assert_eq!(secret::get_by_dotted(&v, "arr.1"), Some(json!("b")));
    assert_eq!(secret::get_by_dotted(&v, "env.NOPE"), None);
    assert_eq!(secret::get_by_dotted(&v, "nope.nope"), None);
}

#[test]
fn 数组里的密钥也会被脱敏() {
    let v = json!({ "tokens": ["sk-one", "sk-two"] });
    let (redacted, secrets) = secret::redact(&v, &[]);
    let s = serde_json::to_string(&redacted).unwrap();
    assert!(!s.contains("sk-one"), "明文泄漏: {s}");
    assert!(secrets.contains(&"tokens.0".to_string()), "{secrets:?}");
    assert!(secrets.contains(&"tokens.1".to_string()), "{secrets:?}");
}
