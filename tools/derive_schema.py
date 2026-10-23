#!/usr/bin/env python3
"""从各家官方来源派生 JSON Schema。

背景：Claude Code / Gemini CLI / OpenCode / Codex CLI 都有官方发布的机器可读
schema，能直接抓。但另外几家没有 —— 它们的 schema 是构建时从 TS/Python 源码
生成的，不落文件。

这里的做法是：**只从官方来源取键位、类型、默认值、说明，不编造**。

- pi          → 官方 npm 包里的 docs/settings.md（Setting|Type|Default|Description 表）
- grok-build  → 官方仓库里的 26-config-reference.md（Key|Type/Values|...|Details 表）
- hermes      → 官方仓库里的 hermes_cli/config_defaults.py（DEFAULT_CONFIG + 注释即文档）
- openclaw    → 官方 npm 包里的 Zod schema（结构/枚举/默认值）＋ 官方生成式文档里的字段说明
- minimax-code→ 官方 npm 包里的配置定义

用法：
    python3 tools/derive_schema.py            # 生成 schemas/*.json
    python3 tools/derive_schema.py --check    # 只校验不写盘
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCHEMA_DIR = os.path.join(ROOT, "schemas")

# ── 通用工具 ──────────────────────────────────────────────

TYPE_MAP = {
    "string": "string", "str": "string", "text": "string", "path": "string",
    "number": "number", "float": "number", "double": "number", "integer": "integer",
    "int": "integer", "boolean": "boolean", "bool": "boolean",
    "array": "array", "list": "array", "table": "object", "object": "object",
    "dict": "object", "map": "object",
}


def jtype(t):
    """把文档里的类型写法归一到 JSON Schema type。"""
    if not t:
        return None
    s = t.strip().lower()
    s = s.split("/")[0].strip()          # "string / string[]" → "string"
    s = re.sub(r"\[.*\]$", "", s).strip()  # "string[]" → "string"
    s = s.replace("of tables", "").replace("of objects", "").strip()
    if s.startswith("array"):
        return "array"
    if s in TYPE_MAP:
        return TYPE_MAP[s]
    return None


# 类型词本身不是枚举候选
_TYPE_WORDS = {
    "string", "number", "integer", "boolean", "array", "object", "table",
    "string[]", "number[]", "integer[]", "boolean[]", "any", "null",
    "array of tables", "array of objects", "list", "map", "dict",
}


def parse_default(text):
    """把文档里的默认值写法解析成 JSON 值（解析不出来就返回 None）。"""
    if text is None:
        return None
    s = strip_md(text).strip()
    s = s.strip("`").strip()
    if not s or s.lower() in ("none", "—", "-", "n/a", "null", "automatic",
                              "required", "optional", "unset", "default"):
        return None
    if s.lower() in ("true", "yes"):
        return True
    if s.lower() in ("false", "no"):
        return False
    if re.fullmatch(r"-?\d+", s):
        return int(s)
    if re.fullmatch(r"-?\d+\.\d+", s):
        return float(s)
    if len(s) >= 2 and s[0] == s[-1] and s[0] in "\"'":
        return s[1:-1]
    # `"streaming"` / `exponential` 这种带反引号的
    if re.fullmatch(r"`[^`]+`", strip_md(text).strip()):
        inner = strip_md(text).strip().strip("`")
        return inner
    return None


def set_desc(node, desc):
    """写 description（已有则不覆盖）。"""
    if not desc:
        return
    desc = " ".join(str(desc).split()).strip()
    if not desc:
        return
    node.setdefault("description", desc)


def deep_set(root, dotted, node):
    """往 schema 里写 `a.b.c` 这样的点分路径（沿途补 object）。

    root 是 schema 根（含 properties），不是 properties 本身。
    """
    parts = [p for p in dotted.split(".") if p]
    if not parts:
        return
    cur = root
    for p in parts[:-1]:
        props = cur.setdefault("properties", {})
        nxt = props.get(p)
        if not isinstance(nxt, dict):
            nxt = {"type": "object", "properties": {}}
            props[p] = nxt
        cur = nxt
    leaf = cur.setdefault("properties", {}).setdefault(parts[-1], {})
    for k, v in node.items():
        if k == "type" and leaf.get("type"):
            continue
        leaf.setdefault(k, v)


def strip_md(s):
    """去掉 markdown 行内标记，留下纯文本。"""
    if s is None:
        return ""
    s = re.sub(r"`([^`]*)`", r"\1", s)
    s = re.sub(r"\*\*([^*]*)\*\*", r"\1", s)
    s = re.sub(r"\*([^*]*)\*", r"\1", s)
    s = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", s)
    return " ".join(s.split()).strip()


def split_row(line):
    """切分 markdown 表格行。

    单元格里的竖线会被写成 `\\|`，直接按 `|` 切会把列拆坏
    （Pi 的类型列就写成 `"off" \\| "streaming" \\| "idle"`）。
    """
    cells = []
    buf = []
    i = 0
    s = line.strip()
    if s.startswith("|"):
        i = 1
    n = len(s)
    while i < n:
        c = s[i]
        if c == "\\" and i + 1 < n and s[i + 1] == "|":
            buf.append("|")
            i += 2
            continue
        if c == "|":
            cells.append("".join(buf).strip())
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    cells.append("".join(buf).strip())
    return cells


def parse_table(md_text):
    """解析 markdown 表格 → (表头, 行列表)。行是单元格文本的列表。"""
    lines = md_text.splitlines()
    tables = []
    i = 0
    while i < len(lines):
        if lines[i].strip().startswith("|") and i + 1 < len(lines) and re.match(
            r"^\s*\|[\s:|-]+\|\s*$", lines[i + 1]
        ):
            header = split_row(lines[i])
            i += 2
            rows = []
            while i < len(lines) and lines[i].strip().startswith("|"):
                cells = split_row(lines[i])
                if len(cells) < len(header):
                    cells += [""] * (len(header) - len(cells))
                rows.append(cells)
                i += 1
            tables.append((header, rows))
        else:
            i += 1
    return tables


def col_index(header, *names):
    """按名字找列号（大小写不敏感）。"""
    low = [h.strip().lower() for h in header]
    for n in names:
        if n in low:
            return low.index(n)
    for i, h in enumerate(low):
        for n in names:
            if n in h:
                return i
    return None


def schema_of(props):
    return {
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": props,
    }


# ── pi：官方 npm 包里的 docs/settings.md ──────────────────

def derive_pi():
    """`| Setting | Type | Default | Description |` 表。"""
    src = os.path.join(
        os.path.expanduser("~"),
        ".pi/agent/settings.md",
    )
    # 优先用本机已安装的官方包
    cands = [src]
    for base in ("/opt/homebrew/lib/node_modules", "/usr/local/lib/node_modules"):
        p = os.path.join(
            base, "@earendil-works/pi-coding-agent/docs/settings.md")
        if os.path.exists(p):
            cands.insert(0, p)
    path = next((c for c in cands if os.path.exists(c)), None)
    if not path:
        return None, "找不到 pi 的 docs/settings.md"

    md = open(path, encoding="utf-8").read()
    schema = schema_of({})
    n = 0
    for header, rows in parse_table(md):
        c_key = col_index(header, "setting", "key", "field", "option")
        c_type = col_index(header, "type")
        c_desc = col_index(header, "description", "purpose", "notes")
        c_def = col_index(header, "default")
        if c_key is None or c_desc is None:
            continue
        for r in rows:
            key = strip_md(r[c_key])
            if not key or key.startswith("-"):
                continue
            t = strip_md(r[c_type]) if c_type is not None else ""
            # 类型列可能是 `"off" \| "streaming" \| "idle"` —— 拆出候选
            tvals = [strip_md(v) for v in t.split("|")]
            tvals = [v.strip('"\'').strip() for v in tvals if v]
            node = {}
            jt = jtype(tvals[0] if tvals else "")
            if jt:
                node["type"] = jt
            # 候选值：排除类型词
            cands = [v for v in tvals
                     if v.lower() not in _TYPE_WORDS and re.fullmatch(r"[A-Za-z0-9_.<>-]+", v)]
            # 一列候选值本身就是枚举（`"off" | "minimal" | ...`）—— 类型是字符串
            if jt is None and len(cands) > 1:
                jt = "string"
                node["type"] = jt
            if jt in ("string", "number", "integer") and len(cands) > 1:
                node["enum"] = cands
            if c_def is not None:
                dv = parse_default(r[c_def])
                if dv is not None:
                    node["default"] = dv
            set_desc(node, strip_md(r[c_desc]))
            deep_set(schema, key, node)
            n += 1
    return schema, f"pi: {n} 个键（来源 {os.path.basename(os.path.dirname(path))}/settings.md）"


# ── grok-build：官方仓库的 26-config-reference.md ─────────

def derive_grok_build():
    """`| Key | Type / Values | Requirements | Managed | Details |` 表。"""
    url = ("https://raw.githubusercontent.com/xai-org/grok-build/main/"
           "crates/codegen/xai-grok-pager/docs/user-guide/26-config-reference.md")
    md = fetch_text(url)
    if md is None:
        return None, "抓不到 grok-build 的 config-reference"

    schema = schema_of({})
    n = 0
    for header, rows in parse_table(md):
        c_key = col_index(header, "key")
        c_type = col_index(header, "type")
        c_desc = col_index(header, "details", "description", "notes")
        if c_key is None or c_desc is None:
            continue
        for r in rows:
            key = strip_md(r[c_key])
            if not key or key.startswith("-"):
                continue
            t = strip_md(r[c_type]) if c_type is not None else ""
            node = {}
            # "string (path)" / "string / string[]" / "array of tables" / "table"
            jt = jtype(t)
            if jt:
                node["type"] = jt
            # 枚举：类型列里的候选（排除类型词本身）
            if jt in ("string", "number", "integer"):
                vals = [strip_md(v) for v in t.split(" / ")]
                vals = [v for v in vals if v and v.lower() not in _TYPE_WORDS]
                if len(vals) > 1 and all(re.fullmatch(r"[A-Za-z0-9_.-]+", v) for v in vals):
                    node["enum"] = vals
            desc = strip_md(r[c_desc])
            # 说明里常带 `(default: `false`)` → 抽成真正的默认值
            dm = re.search(r"default(?:s)?\s*[:=]\s*`?([^`.,;)]+)`?", desc, re.I)
            if dm:
                dv = parse_default(dm.group(1))
                if dv is not None:
                    node["default"] = dv
            set_desc(node, desc)
            deep_set(schema, key, node)
            n += 1
    return schema, f"grok-build: {n} 个键"


# ── hermes：官方仓库的 config_defaults.py ────────────────

def parse_py_assignment(src, name, extra_src=""):
    """用 AST 取模块顶层的某个赋值（不执行代码，避开 import）。"""
    import ast

    try:
        tree = ast.parse(src)
    except SyntaxError:
        return None
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for tgt in node.targets:
                if isinstance(tgt, ast.Name) and tgt.id == name:
                    try:
                        return ast.literal_eval(node.value)
                    except (ValueError, SyntaxError):
                        return eval_py_const(tree, node, extra_src)
    return None


def eval_py_const(tree, node, extra_src=""):
    """求值模块顶层的字面量 dict。

    这些模块会 import 同包里的辅助函数（`_aux(...)` 之类），直接 exec 整个文件
    会连带 import 一堆东西。这里把源码里的顶层函数定义也带上，只在受限的
    命名空间里求值 —— 用到的全是纯数据构造，不碰网络/文件。
    """
    import ast

    keep = []
    for n in tree.body:
        if isinstance(n, ast.Assign):
            keep.append(n)
        elif isinstance(n, ast.FunctionDef) and not n.decorator_list:
            keep.append(n)
    # 辅助函数模块（config_defaults_env）先编译进去，**保持原有顺序** ——
    # 它里面有 `_category(...)` 这种调用，顺序错了会 NameError
    if extra_src:
        try:
            etree = ast.parse(extra_src)
            extra_nodes = [n for n in etree.body
                           if (isinstance(n, ast.FunctionDef) and not n.decorator_list)
                           or isinstance(n, ast.Assign)]
            keep = extra_nodes + keep
        except SyntaxError:
            pass
    mod = ast.Module(body=keep, type_ignores=[])
    try:
        code = compile(mod, "<cfg>", "exec")
    except SyntaxError:
        return None
    ns = {"__builtins__": {"list": list, "dict": dict, "set": set, "tuple": tuple,
                           "str": str, "int": int, "float": float, "bool": bool,
                           "len": len, "sorted": sorted, "enumerate": enumerate,
                           "range": range, "min": min, "max": max, "sum": sum,
                           "object": object, "isinstance": isinstance,
                           "type": type, "abs": abs, "zip": zip,
                           "True": True, "False": False, "None": None}}
    try:
        exec(code, ns)  # noqa: S102
    except Exception:  # noqa: BLE001
        return None
    try:
        return ns.get(node.targets[0].id)
    except Exception:  # noqa: BLE001
        return None


def derive_hermes():
    """DEFAULT_CONFIG 是嵌套 dict；源码里的注释就是官方文档。"""
    url = ("https://raw.githubusercontent.com/NousResearch/hermes-agent/main/"
           "hermes_cli/config_defaults.py")
    src = fetch_text(url)
    if src is None:
        return None, "抓不到 hermes 的 config_defaults.py"

    # DEFAULT_CONFIG 用到同包 config_defaults_env 里的辅助函数，一并带上
    env_url = ("https://raw.githubusercontent.com/NousResearch/hermes-agent/main/"
               "hermes_cli/config_defaults_env.py")
    env_src = fetch_text(env_url) or ""

    cfg = parse_py_assignment(src, "DEFAULT_CONFIG", extra_src=env_src)
    if not isinstance(cfg, dict):
        return None, "DEFAULT_CONFIG 不是 dict（可能解析失败）"

    comments = extract_py_comments(src)
    schema = schema_of({})
    n = 0
    for path, (value, comment) in walk_defaults(cfg, comments):
        node = {}
        t = infer_type(value)
        if t:
            node["type"] = t
        if isinstance(value, (str, int, float, bool)) and value != "" and value is not None:
            node["default"] = value
        set_desc(node, comment)
        deep_set(schema, path, node)
        n += 1
    return schema, f"hermes: {n} 个键"


def infer_type(v):
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        return "array"
    if isinstance(v, dict):
        return "object"
    return None


def extract_py_comments(src):
    """抓 `DEFAULT_CONFIG = {` 之后的注释块 → {点分路径: 说明}。

    注释写在键的上方，缩进和键对齐 —— 按这个规律归位。
    """
    out = {}
    lines = src.splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if l.startswith("DEFAULT_CONFIG"))
    except StopIteration:
        return out

    pending = []
    stack = []          # [(indent, path)]
    for line in lines[start:]:
        if not line.strip() or line.strip().startswith("}"):
            if line.strip().startswith("}"):
                pending = []
            continue
        stripped = line.strip()
        if stripped.startswith("#"):
            pending.append(stripped.lstrip("#").strip())
            continue
        m = re.match(r'^(\s*)"([^"]+)"\s*:', line)
        if not m:
            pending = []
            continue
        indent = len(m.group(1))
        key = m.group(2)
        # 行尾注释：`"max_size": 128,  # LRU entry cap`
        tm = re.search(r",\s*#\s*(.+)$", line)
        if tm and not pending:
            pending = [tm.group(1).strip()]
        while stack and stack[-1][0] >= indent:
            stack.pop()
        path = ".".join([s[1] for s in stack] + [key])
        if pending:
            out[path] = " ".join(pending)
        pending = []
        # 值是 dict 就把这一层压栈
        if re.search(r':\s*\{\s*,?\s*$', line):
            stack.append((indent, path))
    return out


def walk_defaults(cfg, comments, prefix="", extra=None):
    """遍历 DEFAULT_CONFIG，产出 (点分路径, (默认值, 说明))。

    源码里的注释写在**上一级**字典键的上方，但内容常在讲下一级的某个键，
    例如：
        # min_switch_reset_seconds: opt-in (0 = off). …
        "fallback": {"min_switch_reset_seconds": 0},
    这里把注释的首个词（形如 `xxx:`）认出来，归到对应的子键上。
    """
    extra = extra or {}
    for k, v in cfg.items():
        path = f"{prefix}{k}"
        comment = comments.get(path) or extra.get(k, "")
        if isinstance(v, dict):
            # 注释可能是在讲子键
            m = re.match(r"^\s*([A-Za-z_][\w-]*)\s*:", comment)
            child_comment = {}
            if m and m.group(1) in v:
                child_comment[m.group(1)] = comment
            yield from walk_defaults(v, comments, path + ".", child_comment)
            if comment and not child_comment:
                yield path, ({}, comment)
        else:
            yield path, (v, comment)


# ── openclaw：官方 npm 包的 Zod schema ───────────────────

def derive_openclaw():
    """官方包里的 OpenClawSchema（Zod）→ JSON Schema；说明从官方生成式文档补。"""
    pkg = "/tmp/oc-install/node_modules/openclaw"
    if not os.path.isdir(pkg):
        return None, "缺少 openclaw npm 包（先 npm install openclaw --prefix /tmp/oc-install）"

    # 找导出 OpenClawSchema 的 chunk
    chunk = None
    dist = os.path.join(pkg, "dist")
    for f in os.listdir(dist):
        if not f.startswith("zod-schema-") or not f.endswith(".mjs"):
            continue
        txt = open(os.path.join(dist, f), encoding="utf-8", errors="ignore").read()
        if "OpenClawSchema" in txt:
            chunk = os.path.join(dist, f)
            break
    if not chunk:
        return None, "openclaw 包里找不到 zod-schema chunk"

    script = (
        "import { t as OpenClawSchema } from %r;\n"
        "import * as z from 'zod';\n"
        "const j = z.toJSONSchema(OpenClawSchema, { io: 'input', "
        "unrepresentable: 'any', target: 'draft-7' });\n"
        "process.stdout.write(JSON.stringify(j));\n"
    ) % chunk
    with tempfile.NamedTemporaryFile("w", suffix=".mjs", delete=False,
                                     dir=pkg) as tf:
        tf.write(script)
        script_path = tf.name
    try:
        r = subprocess.run(["node", script_path], cwd=pkg, capture_output=True,
                           timeout=300)
        if r.returncode != 0:
            return None, f"导出 openclaw schema 失败: {r.stderr[:200]!r}"
        schema = json.loads(r.stdout.decode("utf-8"))
    finally:
        os.unlink(script_path)

    # 官方生成式文档里的字段说明
    descs = openclaw_doc_descriptions()
    applied = 0
    for path, desc in descs.items():
        node = lookup(schema, path)
        if node is not None:
            set_desc(node, desc)
            applied += 1
    n = count_leaves(schema)
    return schema, f"openclaw: {n} 个叶子键，{applied} 条来自官方文档的说明"


def openclaw_doc_pages():
    base = "https://raw.githubusercontent.com/openclaw/openclaw/main/docs/gateway/"
    return [base + p for p in (
        "config-runtime.md", "config-extensions.md", "config-browser-ui-desktop.md",
        "config-gateway.md", "config-cloud-workers.md", "config-hooks.md",
        "config-secrets-env.md", "config-observability.md", "config-automation.md",
        "config-agents.md", "config-channels.md", "config-tools.md",
    )]


def openclaw_doc_descriptions():
    """从官方生成式文档里抽「路径 → 说明」。

    文档结构固定：
        ## `worktreeRoot`
        Optional global root directory for ... Defaults to `<openclaw-state-dir>/worktrees`.
        - `enabled`: enables TLS termination ... (default: `false`).

    所以有两种来源：
      1) 标题（`## \\`path\\``）后**第一段散文** = 该字段自己的说明
      2) 标题下的 `- \\`leaf\\`: 说明` = 子字段的说明
    """
    out = {}
    for url in openclaw_doc_pages():
        md = fetch_text(url)
        if not md:
            continue
        cur = ""
        lines = md.splitlines()
        for i, line in enumerate(lines):
            hm = re.match(r"^#{2,4}\s+`([a-zA-Z0-9_.\[\]*]+)`\s*$", line)
            if hm:
                cur = hm.group(1)
                # 标题后的第一段散文就是该字段的说明
                desc = first_paragraph(lines, i + 1)
                if desc:
                    out.setdefault(cur, desc)
                continue
            if not cur:
                continue
            bm = re.match(r"^\s*[-*]\s+`([^`]+)`\s*[:：]\s*(.+)$", line)
            if bm:
                leaf, desc = bm.group(1), strip_md(bm.group(2))
                if not desc:
                    continue
                if "." in leaf or "*" in leaf:
                    # bullet 本身就是完整路径（可能带 `*` 通配）
                    out.setdefault(leaf, desc)
                elif cur:
                    out.setdefault(f"{cur}.{leaf}", desc)
    return out


def first_paragraph(lines, start):
    """从 start 行开始找第一段散文（跳过代码块、空行、列表）。"""
    buf = []
    in_code = False
    for line in lines[start:]:
        s = line.strip()
        if s.startswith("```"):
            in_code = not in_code
            if buf:
                break
            continue
        if in_code:
            continue
        if not s:
            if buf:
                break
            continue
        if s.startswith(("-", "*", "|", ">", "#")):
            break
        buf.append(s)
    return strip_md(" ".join(buf)) if buf else ""


def lookup(schema, dotted):
    """按点分路径在 schema 里找节点（支持数组下标与 `*`）。"""
    cur = schema
    for part in dotted.split("."):
        if not part:
            return None
        props = cur.get("properties") if isinstance(cur, dict) else None
        if not props or part not in props:
            return None
        cur = props[part]
    return cur


def count_leaves(schema):
    n = 0

    def walk(node):
        nonlocal n
        if not isinstance(node, dict):
            return
        if "properties" in node:
            for v in node["properties"].values():
                walk(v)
        if "anyOf" in node:
            for b in node["anyOf"]:
                walk(b)
        t = node.get("type")
        if t in ("object", "array"):
            return
        if t or "enum" in node or "const" in node:
            n += 1

    for v in schema.get("properties", {}).values():
        walk(v)
    return n


# ── minimax-code：官方 npm 包里的配置定义 ─────────────────

def derive_minimax_code():
    """官方 npm 包 @minimax-ai/code 的 bundle 里带完整配置默认值。"""
    pkg = "/tmp/mmx/package"
    if not os.path.isdir(pkg):
        return None, "缺少 @minimax-ai/code npm 包"

    obj = None
    for root, _dirs, files in os.walk(pkg):
        for f in files:
            if not f.endswith(".js"):
                continue
            p = os.path.join(root, f)
            try:
                txt = open(p, encoding="utf-8", errors="ignore").read()
            except OSError:
                continue
            if "skipWindowsHardSafetyChecks" not in txt:
                continue
            lit = extract_js_object_literal(txt, "skipWindowsHardSafetyChecks")
            if lit:
                obj = parse_js_object(lit)
                if obj:
                    break
        if obj:
            break
    if not obj:
        return None, "在官方包里找不到 minimax-code 的配置默认值"

    schema = schema_of({})
    n = 0
    for path, value in flatten_js(obj):
        node = {}
        t = infer_type(value)
        if t:
            node["type"] = t
        if isinstance(value, (str, int, float, bool)):
            node["default"] = value
        deep_set(schema, path, node)
        n += 1
    return schema, f"minimax-code: {n} 个键（来自官方包的配置默认值）"


def extract_js_object_literal(txt, anchor):
    """在文本里找出「包含 anchor 的那个对象字面量」的原文（按大括号配平）。"""
    idx = txt.find(anchor)
    if idx < 0:
        return None
    # 往前找最近的 "={" 或 "= {"
    start = txt.rfind("={", 0, idx)
    if start < 0:
        return None
    start = txt.find("{", start)
    depth = 0
    for i in range(start, len(txt)):
        ch = txt[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return txt[start:i + 1]
    return None


_SKIP = object()


def parse_js_object(text):
    """把 JS 对象字面量（minified）解析成 python 结构。

    支持嵌套对象/数组、字符串、数字、布尔；遇到不认识的表达式就返回 None
    （宁可少字段，也不要猜错类型）。
    """
    text = text.strip()
    if not text.startswith("{"):
        return None
    val, idx = _js_value(text, 0)
    if not isinstance(val, dict):
        return None
    return val


def _js_value(s, i):
    """从 s[i] 开始解析一个 JS 值，返回 (值, 下一个位置)。"""
    while i < len(s) and s[i] in " \t\r\n":
        i += 1
    if i >= len(s):
        return None, i
    c = s[i]
    if c == "{":
        obj = {}
        i += 1
        while i < len(s):
            while i < len(s) and s[i] in " \t\r\n,":
                i += 1
            if i < len(s) and s[i] == "}":
                return obj, i + 1
            if s.startswith("...", i):
                # 对象展开：`...标识符` 或 `...{...}` / `...[...]`
                ident = re.match(r"\.\.\.[A-Za-z_$][\w$]*", s[i:])
                if ident:
                    i += ident.end()
                    continue
                _v, i = _js_value(s, i + 3)
                continue
            m = re.match(r"([A-Za-z_$][\w$]*)\s*:\s*", s[i:])
            if not m:
                # 认不出来的片段：跳到下一个顶层逗号，继续解析后面的键
                depth = 0
                j = i
                while j < len(s):
                    ch = s[j]
                    if ch in "{[(":
                        depth += 1
                    elif ch in "}])":
                        if depth == 0:
                            break
                        depth -= 1
                    elif ch == "," and depth == 0:
                        break
                    j += 1
                i = j + 1
                continue
            key = m.group(1)
            val, i = _js_value(s, i + m.end())
            if val is _SKIP:
                continue          # 这个值认不出来，跳过这个键，不影响其它键
            obj[key] = val
        return obj, i
    if c == "[":
        arr = []
        i += 1
        while i < len(s):
            while i < len(s) and s[i] in " \t\r\n,":
                i += 1
            if i < len(s) and s[i] == "]":
                return arr, i + 1
            val, i = _js_value(s, i)
            if val is _SKIP:
                return None, i
            arr.append(val)
        return arr, i
    m = re.match(r'"((?:[^"\\]|\\.)*)"', s[i:])
    if m:
        return m.group(1).encode().decode("unicode_escape"), i + m.end()
    m = re.match(r"'((?:[^'\\]|\\.)*)'", s[i:])
    if m:
        return m.group(1), i + m.end()
    m = re.match(r"(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)", s[i:])
    if m:
        raw = m.group(1)
        end = i + m.end()
        # 后面跟着运算符（`1800*1e3`）→ 整段表达式当成一个值
        k = end
        while k < len(s) and s[k] in " \t":
            k += 1
        if k < len(s) and s[k] in "*+-/%":
            depth = 0
            while k < len(s):
                ch = s[k]
                if ch in "{[(":
                    depth += 1
                elif ch in "}])":
                    if depth == 0:
                        break
                    depth -= 1
                elif ch == "," and depth == 0:
                    break
                k += 1
            end = k
        try:
            val = float(raw) if ("." in raw or "e" in raw.lower()) else int(raw)
        except ValueError:
            val = None
        return val, end
    if s.startswith("!0", i):
        return False, i + 2
    if s.startswith("!1", i):
        return True, i + 2
    if s.startswith("void 0", i) or s.startswith("undefined", i) or s.startswith("null", i):
        return None, i + len(s[i:].split(",")[0])
    # 认不出来的表达式（函数调用、标识符…）→ 跳过这个键
    depth = 0
    j = i
    while j < len(s):
        ch = s[j]
        if ch in "{[(":
            depth += 1
        elif ch in "}])":
            if depth == 0:
                break
            depth -= 1
        elif ch == "," and depth == 0:
            break
        j += 1
    return _SKIP, j


def flatten_js(obj, prefix=""):
    for k, v in obj.items():
        path = f"{prefix}{k}"
        if isinstance(v, dict):
            yield from flatten_js(v, path + ".")
        else:
            yield path, v


# ── HTTP 小工具 ───────────────────────────────────────────

def fetch_text(url, timeout=40, retries=3):
    """抓文本；GitHub raw 偶尔限流，重试几次。"""
    for attempt in range(retries):
        try:
            r = subprocess.run(
                ["curl", "-s", "--compressed", "-m", str(timeout), "-A",
                 "cfg-explorer/0.1 (schema deriver)", url],
                capture_output=True, timeout=timeout + 10)
            if r.returncode == 0 and r.stdout:
                return r.stdout.decode("utf-8", errors="ignore")
        except Exception:  # noqa: BLE001
            pass
        time.sleep(2 * (attempt + 1))
    return None


# ── 主流程 ────────────────────────────────────────────────

DERIVERS = {
    "pi": derive_pi,
    "grok-build": derive_grok_build,
    "hermes": derive_hermes,
    "openclaw": derive_openclaw,
    "minimax-code": derive_minimax_code,
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只校验不写盘")
    ap.add_argument("--only", default="", help="只处理某几个（逗号分隔）")
    args = ap.parse_args()

    os.makedirs(SCHEMA_DIR, exist_ok=True)
    want = [x.strip() for x in args.only.split(",") if x.strip()] or list(DERIVERS)

    rc = 0
    for name in want:
        fn = DERIVERS.get(name)
        if not fn:
            print(f"  ? {name}: 没有派生器")
            continue
        schema, note = fn()
        if schema is None:
            print(f"  ✗ {name}: {note}")
            rc = 1
            continue
        props = schema.get("properties", {})
        leaves = count_leaves(schema)
        print(f"  ✓ {name}: {note}")
        if not props:
            print(f"      警告：没有解析出任何属性")
            rc = 1
            continue
        out = os.path.join(SCHEMA_DIR, f"{name}.json")
        if args.check:
            cur = json.load(open(out, encoding="utf-8")) if os.path.exists(out) else None
            same = cur == schema
            print(f"      {'一致' if same else '与磁盘不一致'}（磁盘上 {len(cur.get('properties', {})) if cur else 0} 个顶层属性）")
        else:
            with open(out, "w", encoding="utf-8") as f:
                json.dump(schema, f, ensure_ascii=False, indent=1, sort_keys=True)
            print(f"      写入 {os.path.relpath(out, ROOT)}（{leaves} 个叶子键）")
    return rc


if __name__ == "__main__":
    sys.exit(main())
