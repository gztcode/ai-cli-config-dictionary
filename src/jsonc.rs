//! 自写的 JSON / JSONC 解析器：**记录每个值的字节区间**。
//!
//! 为什么不用现成的库：`jsonc-parser` 这类库只给「值」，不给字节偏移；
//! 而保格式编辑必须有偏移才能做到「只改用户动的那几个字节」。
//!
//! 支持：标准 JSON、JSONC（`//` 与 `/* */` 注释）、尾逗号、松散键名。

use anyhow::{anyhow, Result};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

#[derive(Debug, Clone)]
pub struct JMember {
    pub key: String,
    /// 整个成员（key + 值）的字节区间
    pub start: usize,
    pub end: usize,
    pub value: JNode,
}

#[derive(Debug, Clone)]
pub struct JNode {
    pub kind: JKind,
    /// 值文本的字节区间 [start, end)
    pub start: usize,
    pub end: usize,
    /// 对象成员
    pub members: Vec<JMember>,
    /// 数组元素
    pub items: Vec<JNode>,
    /// 解析出来的值
    pub value: Value,
}

impl JNode {
    /// 按 key 找成员
    pub fn member(&self, key: &str) -> Option<&JMember> {
        self.members.iter().find(|m| m.key == key)
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn new(s: &'a str) -> Self {
        P {
            b: s.as_bytes(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    /// 跳过空白与注释
    fn skip_ws(&mut self) {
        loop {
            while let Some(c) = self.peek() {
                if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                    self.i += 1;
                } else {
                    break;
                }
            }
            // 注释
            if self.peek() == Some(b'/') {
                if self.b.get(self.i + 1) == Some(&b'/') {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        if c == b'\n' {
                            break;
                        }
                    }
                    continue;
                }
                if self.b.get(self.i + 1) == Some(&b'*') {
                    self.i += 2;
                    while self.i + 1 < self.b.len() {
                        if self.b[self.i] == b'*' && self.b[self.i + 1] == b'/' {
                            self.i += 2;
                            break;
                        }
                        self.i += 1;
                    }
                    continue;
                }
            }
            break;
        }
    }

    fn eat(&mut self, c: u8) -> Result<()> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(anyhow!(
                "第 {} 字节处期望 '{}'，实际 {:?}",
                self.i,
                c as char,
                self.peek().map(|x| x as char)
            ))
        }
    }

    fn parse_value(&mut self) -> Result<JNode> {
        self.skip_ws();
        let start = self.i;
        let c = self.peek().ok_or_else(|| anyhow!("内容意外结束"))?;

        match c {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => {
                let s = self.parse_string()?;
                Ok(JNode {
                    kind: JKind::String,
                    start,
                    end: self.i,
                    members: vec![],
                    items: vec![],
                    value: Value::String(s),
                })
            }
            b't' | b'f' | b'n' => {
                let word = if c == b't' {
                    "true"
                } else if c == b'f' {
                    "false"
                } else {
                    "null"
                };
                if self.b[self.i..].starts_with(word.as_bytes()) {
                    self.i += word.len();
                    let v = match word {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => Value::Null,
                    };
                    Ok(JNode {
                        kind: if word == "null" {
                            JKind::Null
                        } else {
                            JKind::Bool
                        },
                        start,
                        end: self.i,
                        members: vec![],
                        items: vec![],
                        value: v,
                    })
                } else {
                    Err(anyhow!("第 {start} 字节处不是合法的字面量"))
                }
            }
            _ => self.parse_number(),
        }
    }

    fn parse_object(&mut self) -> Result<JNode> {
        let start = self.i;
        self.eat(b'{')?;
        let mut members = Vec::new();
        let mut map = serde_json::Map::new();

        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                Some(b',') => {
                    // 尾逗号：吃掉逗号后如果下一个是 } 就结束
                    self.i += 1;
                    self.skip_ws();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        break;
                    }
                    continue;
                }
                None => return Err(anyhow!("对象未闭合")),
                _ => {}
            }

            let member_start = self.i;
            self.skip_ws();
            let key = if self.peek() == Some(b'"') {
                self.parse_string()?
            } else {
                // 松散键名（未加引号）
                let ks = self.i;
                while let Some(c) = self.peek() {
                    if c == b':' || c.is_ascii_whitespace() {
                        break;
                    }
                    self.i += 1;
                }
                String::from_utf8_lossy(&self.b[ks..self.i]).to_string()
            };
            self.skip_ws();
            self.eat(b':')?;
            let value = self.parse_value()?;
            let member_end = self.i;

            map.insert(key.clone(), value.value.clone());
            members.push(JMember {
                key,
                start: member_start,
                end: member_end,
                value,
            });
        }

        Ok(JNode {
            kind: JKind::Object,
            start,
            end: self.i,
            members,
            items: vec![],
            value: Value::Object(map),
        })
    }

    fn parse_array(&mut self) -> Result<JNode> {
        let start = self.i;
        self.eat(b'[')?;
        let mut items = Vec::new();
        let mut arr = Vec::new();

        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => {
                    self.i += 1;
                    break;
                }
                Some(b',') => {
                    self.i += 1;
                    self.skip_ws();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        break;
                    }
                    continue;
                }
                None => return Err(anyhow!("数组未闭合")),
                _ => {}
            }
            let v = self.parse_value()?;
            arr.push(v.value.clone());
            items.push(v);
        }

        Ok(JNode {
            kind: JKind::Array,
            start,
            end: self.i,
            members: vec![],
            items,
            value: Value::Array(arr),
        })
    }

    fn parse_string(&mut self) -> Result<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(anyhow!("字符串未闭合"));
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(anyhow!("转义符后内容意外结束"));
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .b
                                .get(self.i..self.i + 4)
                                .ok_or_else(|| anyhow!("\\u 转义不完整"))?;
                            let hex = std::str::from_utf8(hex)?;
                            let code = u32::from_str_radix(hex, 16)?;
                            self.i += 4;
                            // 代理对
                            if (0xD800..0xDC00).contains(&code)
                                && self.b.get(self.i) == Some(&b'\\')
                                && self.b.get(self.i + 1) == Some(&b'u')
                            {
                                let hex2 = self
                                    .b
                                    .get(self.i + 2..self.i + 6)
                                    .ok_or_else(|| anyhow!("代理对不完整"))?;
                                let hex2 = std::str::from_utf8(hex2)?;
                                let low = u32::from_str_radix(hex2, 16)?;
                                self.i += 6;
                                let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                            } else {
                                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            }
                        }
                        other => out.push(other as char),
                    }
                }
                _ => {
                    // 原样收集 UTF-8
                    let s = self.i - 1;
                    let len = utf8_len(c);
                    self.i = s + len;
                    let chunk = self
                        .b
                        .get(s..self.i)
                        .ok_or_else(|| anyhow!("UTF-8 字节不完整"))?;
                    out.push_str(&String::from_utf8_lossy(chunk));
                }
            }
        }
        Ok(out)
    }

    fn parse_number(&mut self) -> Result<JNode> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                self.i += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i])?;
        let v: Value = serde_json::from_str(text).map_err(|e| anyhow!("数字解析失败: {e}"))?;
        Ok(JNode {
            kind: JKind::Number,
            start,
            end: self.i,
            members: vec![],
            items: vec![],
            value: v,
        })
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// 解析 JSON / JSONC
pub fn parse(text: &str) -> Result<JNode> {
    let mut p = P::new(text);
    let node = p.parse_value()?;
    p.skip_ws();
    if p.i < p.b.len() {
        return Err(anyhow!("第 {} 字节处有多余内容", p.i));
    }
    Ok(node)
}

/// 查找 path 对应的节点
pub fn find<'a>(root: &'a JNode, path: &[String]) -> Option<&'a JNode> {
    if path.is_empty() {
        return Some(root);
    }
    match root.kind {
        JKind::Object => {
            let m = root.member(&path[0])?;
            find(&m.value, &path[1..])
        }
        JKind::Array => {
            let idx: usize = path[0].parse().ok()?;
            let it = root.items.get(idx)?;
            find(it, &path[1..])
        }
        _ => None,
    }
}
