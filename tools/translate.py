#!/usr/bin/env python3
"""把 schema 里的英文说明批量翻译成简体中文，覆盖层落 src/i18n/zh-CN.json。

设计要点：
- 按「唯一英文文本」翻译，再映射回 tool:path（1537 条 → 857 条唯一文本，术语更统一）
- 增量：源文本变了才重译（tools/.translate-cache.json 存 hash）
- 质量闸门：占位符/反引号/环境变量名必须原样保留，不一致则重试，仍失败就保留英文
- 并发调用，带退避重试

用法：
  python3 tools/translate.py                 # 增量翻译缺失的
  python3 tools/translate.py --check         # 只校验现有覆盖层，不调模型
  python3 tools/translate.py --force         # 忽略缓存全部重译
  python3 tools/translate.py --limit 50      # 只处理前 50 条（调试用）
"""

import argparse
import concurrent.futures
import glob
import hashlib
import json
import os
import re
import sys
import threading
import time

try:
    import httpx
except ImportError:
    sys.exit("缺 httpx：pip install httpx")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
IR_GLOB = os.path.join(ROOT, "out", "ir", "*.json")
OUT_FILE = os.path.join(ROOT, "src", "i18n", "zh-CN.json")
CACHE_FILE = os.path.join(ROOT, "tools", ".translate-cache.json")
AUTH_FILE = os.path.expanduser("~/.minimax/auth/prod/cn/mcode-public/auth.json")
ENDPOINT = "https://agent.minimax.cn/mavis/api/v1/llm/v1/messages"
MODEL = "minimax/MiniMax-M3.1-Flash"

# ── 术语表：保证同一术语全局一致 ────────────────────────────────
GLOSSARY = {
    "model": "模型", "models": "模型", "permission": "权限", "permissions": "权限",
    "hook": "钩子", "hooks": "钩子", "telemetry": "遥测", "session": "会话",
    "sessions": "会话", "agent": "智能体", "agents": "智能体", "prompt": "提示词",
    "prompts": "提示词", "context window": "上下文窗口", "rate limit": "速率限制",
    "fallback": "回退", "sandbox": "沙箱", "checkpoint": "检查点", "plugin": "插件",
    "plugins": "插件", "workflow": "工作流", "command": "命令", "commands": "命令",
    "environment variable": "环境变量", "env": "环境变量", "tool": "工具",
    "tools": "工具", "notification": "通知", "output": "输出", "input": "输入",
    "default": "默认", "schema": "架构", "endpoint": "端点", "proxy": "代理",
    "experimental": "实验性", "deprecated": "已废弃", "workspace": "工作区",
    "streaming": "流式", "token": "令牌", "tokens": "令牌", "timeout": "超时",
    "authentication": "身份验证", "credential": "凭据", "credentials": "凭据",
    "certificate": "证书", "directory": "目录", "server": "服务器", "servers": "服务器",
    "search": "搜索", "format": "格式", "language": "语言", "theme": "主题",
    "experimental warning": "实验性警告", "status line": "状态栏",
    "system prompt": "系统提示词", "memory": "记忆", "subagent": "子智能体",
    "webhook": "Webhook", "callback": "回调", "buffer": "缓冲区", "priority": "优先级",
    "auto": "自动", "custom": "自定义", "global": "全局", "project": "项目",
    "user": "用户", "allowed": "允许", "denied": "拒绝", "ask": "询问",
    "launch": "启动", "config": "配置", "configuration": "配置", "settings": "设置",
    "enable": "启用", "disable": "禁用", "enabled": "启用", "disabled": "禁用",
    "current": "当前", "previous": "上一个", "next": "下一个", "all": "全部",
    "none": "无", "optional": "可选", "required": "必填", "number": "数字",
    "string": "字符串", "boolean": "布尔值", "array": "数组", "object": "对象",
    "undocumented": "未文档化", "insert mode": "插入模式",
}

SYSTEM = """你是 AI CLI 配置项说明文档的本地化译者，负责把英文说明翻译成简体中文。

翻译规则：
1. 只输出译文本身，不要解释、不要加引号、不要复述原文。
2. 以下内容必须原样保留、不得翻译、不得改动大小写：
   - 反引号包裹的内容，如 `model`、`permissions.allow`
   - 全大写的环境变量名，如 ANTHROPIC_API_KEY、ANTHROPIC_BASE_URL
   - 文件路径、URL、JSON 键名、命令名
   - 尖括号占位符，如 <value>、{name}
   注意：DEPRECATED、UNDOCUMENTED 这类说明性标记词要翻译成「已废弃」「未文档化」，
   它们不是标识符。
3. 保留原文的 Markdown 标记：粗体 **x**、斜体 *x*、行内代码、列表结构。
4. 术语严格按下表翻译（表外的术语按通顺的中文技术文档习惯处理）：
{glossary}
5. 语气是简洁的说明书口吻。只翻译原文已有的信息，绝不添加解释、示例或建议。
6. 原文很短时（如 "Hook type"），译文同样保持简短，不要扩写成句子。
7. 原文已是中文或不含英文单词时，原样返回。
"""

_lock = threading.Lock()
_client = None


def client():
    """复用连接池：这个接口在高并发下会排队，必须少发多复用。"""
    global _client
    if _client is None:
        _client = httpx.Client(
            limits=httpx.Limits(max_connections=4, max_keepalive_connections=4),
            timeout=httpx.Timeout(300.0, connect=30.0),
        )
    return _client
_stats = {"done": 0, "cached": 0, "failed": 0, "retried": 0}


def token():
    auth = json.load(open(AUTH_FILE, encoding="utf-8"))
    rec = list(auth["records"].values())[0]
    return rec["accessToken"]


def glossary_block():
    # 长的放前面，短的会被长词覆盖时反而更自然；这里按词长降序排
    items = sorted(GLOSSARY.items(), key=lambda kv: -len(kv[0]))
    return "\n".join(f"- {en} → {zh}" for en, zh in items)


# ── 质量闸门：算出译文里必须原样保留的片段 ────────────────────────
# 最初的规则是「所有全大写 token 一律保留」，本意是保护 ANTHROPIC_API_KEY 这类
# 环境变量名，但会误伤两类东西：
#   1) 用大写强调的普通英文词（ONLY、REPLACE）—— 模型翻译成中文反被判不合格
#   2) 缩写（JSON、OTLP）—— 反而必须保留
# 现在的判定是「像不像标识符」：
#   含下划线或数字 → 环境变量 / 十六进制色值 / 日期格式
#   前带 $        → shell 变量或占位符（$VISUAL、$ARGUMENTS）
#   后跟 .md 等   → 文件名（CLAUDE.md）
#   不在英文词典里 → 缩写或品牌名（JSON、OTLP、MITM、YOLO）
#   其余（ONLY / REPLACE / VISUAL 这类普通英文词）→ 应当本地化
_TOKEN_RE = re.compile(r"`[^`]*`|<[^>]+>|\{[^}]+\}|\b[A-Z][A-Z0-9_]{3,}\b")
_FILE_EXT_RE = re.compile(r"\.[A-Za-z]{1,4}\b")

# shell / git 的标准名，即使也是普通英文词也要保留
_PRESERVE_EXACT = {
    "PATH", "HEAD", "HOME", "USER", "SHELL", "PWD", "ORIG_HEAD", "HTTPS", "HTTP",
}

# 真正的缩写/术语，全大写出现时必须原样保留。
#
# 之前用的是「不在英文词典里 → 当作标识符」这条规则，结果把
# DISCOURAGED / IGNORED / PAID / SKIPPED 这类**用大写强调的普通英文词**
# 也当成了标识符，模型一翻译就判不合格 —— 有 14 条说明因此永远翻不出来。
# 改成白名单：只有确定是缩写/术语的才要求保留，其余全大写词都允许本地化。
_ACRONYMS = {
    "JSON", "POST", "HTTP", "HTTPS", "OTLP", "VRAM", "CLI", "API", "URL", "MCP",
    "LLM", "TLS", "SSH", "TUI", "IDE", "ISO", "SHA", "ANSI", "CORS", "UUID",
    "SOCKS", "MITM", "HKCU", "HKLM", "YAML", "TOML", "CPU", "GPU", "RAM",
    "SSE", "WS", "OAUTH", "JWT", "CSV", "XML", "HTML", "CSS", "SDK", "UTC",
    "CHANGELOG", "YOLO", "YAML", "PNG", "UTF", "IPC", "PID", "TTL", "GREP",
}

# 词典缺失时的兜底词表（覆盖最常见的英文词）
_FALLBACK_WORDS = {
    "only", "replace", "insert", "argument", "arguments", "visual", "editor",
    "head", "path", "default", "deprecated", "undocumented", "example", "note",
    "warning", "required", "optional", "true", "false", "enabled", "disabled",
    "allow", "deny", "ask", "auto", "manual", "custom", "global", "project",
    "user", "none", "all", "any", "other", "others", "must", "should", "may",
    "can", "will", "see", "use", "set", "not", "and", "or", "the", "this",
    "that", "with", "from", "when", "where", "which", "that", "list", "name",
    "value", "type", "key", "file", "files", "directory", "string", "number",
    "boolean", "array", "object", "level", "mode", "state", "size", "count",
}

_DICT_PATH = "/usr/share/dict/words"
_EN_WORDS = None


def _en_words():
    """英文单词表（优先用系统词典，缺失时用兜底表）。"""
    global _EN_WORDS
    if _EN_WORDS is None:
        words = set(_FALLBACK_WORDS)
        try:
            with open(_DICT_PATH, encoding="utf-8", errors="ignore") as f:
                for line in f:
                    w = line.strip().lower()
                    if w and w.isalpha():
                        words.add(w)
        except OSError:
            pass
        _EN_WORDS = words
    return _EN_WORDS


def keepset(text):
    """返回译文必须原样保留的片段（按上下文判定，不只看大小写）。"""
    text = text or ""
    words = _en_words()
    out = set()
    for m in _TOKEN_RE.finditer(text):
        t = m.group(0)
        if t[0] in "`<{":
            out.add(t)
            continue
        prev = text[m.start() - 1] if m.start() else ""
        nxt = text[m.end():m.end() + 4]
        if "_" in t or any(c.isdigit() for c in t):
            out.add(t)
        elif prev == "$":
            out.add(t)                       # $VISUAL / $ARGUMENTS
        elif _FILE_EXT_RE.match(nxt):
            out.add(t)                       # CLAUDE.md
        elif t in _PRESERVE_EXACT:
            out.add(t)                       # shell / git 标准名
        elif t in _ACRONYMS:
            out.add(t)                       # 真缩写 / 术语
        # 其余全大写词（DISCOURAGED / IGNORED / ONLY / REPLACE …）是强调用法
        # → 允许本地化
    return sorted(out)


def parse_json_array(raw):
    raw = raw.strip()
    if raw.startswith("```"):
        raw = re.sub(r"^```[a-zA-Z]*\s*", "", raw)
        raw = re.sub(r"\s*```$", "", raw)
    raw = raw.strip()
    start = raw.find("[")
    end = raw.rfind("]")
    if start < 0 or end < 0:
        raise ValueError("响应里找不到 JSON 数组")
    return json.loads(raw[start:end + 1])


def call_llm(tok, batch, timeout=300, retries=3):
    """batch: [(id, english)] -> {id: chinese}"""
    payload = [{"id": i, "text": t} for i, t in batch]
    body = {
        "model": "MiniMax-M3.1-Flash",
        "max_tokens": 16000,
        "temperature": 0.2,
        # 翻译是机械任务，低推理档又快又稳（实测 26s → 2s）
        "reasoning_effort": "low",
        # 注意：M3.1-Flash 强制 adaptive thinking，不能显式关闭；
        # 响应里 content 会同时含 thinking 块与 text 块，只取 text。
        "system": SYSTEM.replace("{glossary}", glossary_block()),
        "messages": [{
            "role": "user",
            "content": "把下面每条说明翻译成简体中文。只输出 JSON 数组，"
                       "格式为 [{\"id\": 1, \"zh\": \"译文\"}, ...]，条目数与 id 必须与输入一一对应。\n\n"
                       + json.dumps(payload, ensure_ascii=False),
        }],
    }
    last = None
    for attempt in range(retries):
        try:
            r = client().post(ENDPOINT, json=body, headers={
                "Authorization": f"Bearer {tok}",
                "Content-Type": "application/json",
                "anthropic-version": "2023-06-01",
            })
            if r.status_code != 200:
                last = f"HTTP {r.status_code}: {r.text[:200]}"
                # 401 = token 过期了：重新读一次认证文件再试
                if r.status_code == 401:
                    try:
                        tok = token()
                        last = "token 已刷新"
                    except Exception as e:  # noqa: BLE001
                        last = f"token 刷新失败: {e!r}"
                time.sleep(1.5 * (attempt + 1))
                continue
            d = r.json()
            text = "".join(c.get("text", "") for c in d.get("content", []) if c.get("type") == "text")
            try:
                items = parse_json_array(text)
            except Exception as e:  # noqa: BLE001
                # 抓一次原始响应到磁盘，方便事后定位（模型只回 thinking / 截断 / 换了格式）
                try:
                    with open("/tmp/llm-debug.jsonl", "a", encoding="utf-8") as f:
                        f.write(json.dumps({
                            "err": repr(e),
                            "blocks": [c.get("type") for c in d.get("content", [])],
                            "text_len": len(text),
                            "text_head": text[:400],
                            "raw_keys": list(d.keys()),
                        }, ensure_ascii=False) + "\n")
                except OSError:
                    pass
                last = f"解析失败: {e!r}（内容块 {[c.get('type') for c in d.get('content', [])]}）"
                time.sleep(1.5 * (attempt + 1))
                continue
            out = {}
            for it in items:
                try:
                    out[int(it["id"])] = str(it["zh"]).strip()
                except (KeyError, TypeError, ValueError):
                    pass
            if len(out) != len(batch):
                last = f"返回条目数不符：期望 {len(batch)}，实得 {len(out)}"
                time.sleep(1.5 * (attempt + 1))
                continue
            return out, None
        except Exception as e:  # noqa: BLE001
            last = repr(e)
            time.sleep(1.5 * (attempt + 1))
    return None, last


def translate_batch(tok, batch, quality_retries=2):
    """翻译一批，带占位符质量闸门。返回 {text: zh}，不合格的条目不写入。"""
    # batch 是纯文本列表；这里编号成 (id, text)
    numbered = [(i, en) for i, en in enumerate(batch, start=1)]
    # 每批都重新读一次 token —— OAuth token 约 1 小时过期，
    # 长跑时拿着启动时的旧 token 会整片 401。
    try:
        tok = token()
    except Exception:  # noqa: BLE001
        pass
    out, err = call_llm(tok, numbered)
    if out is None:
        return {}, err
    good = {}
    bad = []
    for i, en in numbered:
        zh = out.get(i)
        if not zh:
            bad.append((i, en, None))
            continue
        # 质量闸门：必须保留全部需原样片段
        miss = [k for k in keepset(en) if k not in zh]
        if miss:
            bad.append((i, en, zh))
        else:
            good[en] = zh
    # 质量不合格的重译一次（单独重试，便于定位）
    if bad:
        with _lock:
            _stats["retried"] += len(bad)
        retry_batch = [(bid, en) for bid, en, _ in bad]
        out2, err2 = call_llm(tok, retry_batch)
        if out2:
            for i, (bid, en) in enumerate(retry_batch, start=1):
                zh = out2.get(i)
                if zh and all(k in zh for k in keepset(en)):
                    good[en] = zh
                else:
                    with _lock:
                        _stats["failed"] += 1
        else:
            with _lock:
                _stats["failed"] += len(bad)
    return good, None


def collect_entries():
    """读 IR，收集 (tool, path, description) 与唯一英文文本。

    注意：这里收的是**所有**带说明的键，不只是叶子键。
    字典页会给对象节点也渲染说明文字，只译叶子键会留下 389 个英文节点。
    """
    entries = []          # (tool, path, desc)
    uniq = {}             # desc -> None
    for p in sorted(glob.glob(IR_GLOB)):
        d = json.load(open(p, encoding="utf-8"))
        tool = d["tool"]

        def walk(keys):
            for k in keys:
                desc = (k.get("description") or "").strip()
                if desc:
                    entries.append((tool, k["path"], desc))
                    uniq[desc] = None
                walk(k.get("children") or [])

        walk(d["keys"])
    return entries, list(uniq.keys())


def load_cache():
    if os.path.exists(CACHE_FILE):
        try:
            return json.load(open(CACHE_FILE, encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {}
    return {}


def save_cache(cache):
    os.makedirs(os.path.dirname(CACHE_FILE), exist_ok=True)
    with open(CACHE_FILE, "w", encoding="utf-8") as f:
        json.dump(cache, f, ensure_ascii=False, indent=1, sort_keys=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只校验，不调模型")
    ap.add_argument("--force", action="store_true", help="忽略缓存全部重译")
    ap.add_argument("--limit", type=int, default=0, help="只处理前 N 条唯一文本")
    ap.add_argument("--batch", type=int, default=20, help="每批条数")
    ap.add_argument("--workers", type=int, default=2, help="并发数（该接口高并发会排队超时，别调大）")
    ap.add_argument("--dry", action="store_true", help="只跑通流程不落盘")
    args = ap.parse_args()

    entries, uniq_texts = collect_entries()
    print(f"待译条目 {len(entries)} 条，唯一英文文本 {len(uniq_texts)} 条")

    cache = load_cache()
    def h(t):
        return hashlib.sha256(t.encode("utf-8")).hexdigest()[:16]

    if args.force:
        todo = list(uniq_texts)
    else:
        todo = [t for t in uniq_texts if cache.get(t, {}).get("h") != h(t)]
    cached_cnt = len(uniq_texts) - len(todo)
    if args.limit:
        todo = todo[: args.limit]
    print(f"需翻译 {len(todo)} 条（已缓存 {cached_cnt} 条）")

    if args.check:
        # 校验现有覆盖层：占位符是否保留
        cov = json.load(open(OUT_FILE, encoding="utf-8")) if os.path.exists(OUT_FILE) else {}
        bad = 0
        for tool, path, desc in entries:
            k = f"{tool}:{path}"
            zh = cov.get(k)
            if not zh:
                continue
            miss = [x for x in keepset(desc) if x not in zh]
            if miss:
                bad += 1
                print(f"  ✗ {k} 丢失: {miss}")
        print(f"覆盖层条目 {len(cov)}，占位符异常 {bad}")
        return 0

    if not todo:
        print("无需翻译")
    else:
        try:
            tok = token()
        except Exception as e:  # noqa: BLE001
            sys.exit(f"取不到 token：{e}")

        batches = [todo[i:i + args.batch] for i in range(0, len(todo), args.batch)]
        total = len(batches)
        done = 0
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
            futs = {ex.submit(translate_batch, tok, b): b for b in batches}
            for fut in concurrent.futures.as_completed(futs):
                b = futs[fut]
                try:
                    good, err = fut.result()
                except Exception as e:  # noqa: BLE001 —— 单批失败不能拖垮整轮
                    good, err = {}, f"批次异常: {e!r}"
                with _lock:
                    for en, zh in good.items():
                        cache[en] = {"h": h(en), "zh": zh}
                    _stats["done"] += len(good)
                done += 1
                print(f"  [{done}/{total}] +{len(good)}/{len(b)}"
                      + (f"  err={err}" if err else "")
                      + f"  累计={_stats['done']} 失败={_stats['failed']}")
                save_cache(cache)

        save_cache(cache)

    # 生成覆盖层
    cov = {}
    for tool, path, desc in entries:
        rec = cache.get(desc)
        if not rec:
            continue
        zh = rec.get("zh")
        if zh:
            cov[f"{tool}:{path}"] = zh
    cov["__meta__"] = json.dumps(
        {"generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
         "entries": len(cov) - 1, "model": MODEL}, ensure_ascii=False)

    if not args.dry:
        os.makedirs(os.path.dirname(OUT_FILE), exist_ok=True)
        with open(OUT_FILE, "w", encoding="utf-8") as f:
            json.dump(cov, f, ensure_ascii=False, indent=1, sort_keys=True)
    print(f"覆盖层写入 {OUT_FILE}：{len(cov) - 1} 条")
    print(f"统计：成功 {_stats['done']}  重试 {_stats['retried']}  失败 {_stats['failed']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
