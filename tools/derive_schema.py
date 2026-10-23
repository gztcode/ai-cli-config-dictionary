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
    for _heading, header, rows in parse_tables_by_section(md_text):
        yield (header, rows)


def parse_tables_by_section(md_text):
    """解析 markdown 表格，并记住每张表上方最近的小节标题。

    产出 (小节标题, 表头, 行列表)。小节标题用来给每个键生成
    「文档章节级锚点」—— 官方文档没有逐键链接时，至少能跳到对的那一节。
    """
    lines = md_text.splitlines()
    tables = []
    heading = ""
    i = 0
    while i < len(lines):
        line = lines[i]
        hm = re.match(r"^#{1,6}\s+(.*)$", line)
        if hm:
            heading = strip_md(hm.group(1)).strip()
            i += 1
            continue
        if line.strip().startswith("|") and i + 1 < len(lines) and re.match(
            r"^\s*\|[\s:|-]+\|\s*$", lines[i + 1]
        ):
            header = split_row(line)
            i += 2
            rows = []
            while i < len(lines) and lines[i].strip().startswith("|"):
                cells = split_row(lines[i])
                if len(cells) < len(header):
                    cells += [""] * (len(header) - len(cells))
                rows.append(cells)
                i += 1
            tables.append((heading, header, rows))
        else:
            i += 1
    return tables


def gh_slug(text):
    """GitHub 标题锚点：小写、去标点、空格转连字符。

    `### \\`agent\\`` → agent；`## Model and thinking` → model-and-thinking
    """
    s = strip_md(text).strip().lower()
    s = s.replace("`", "")
    s = re.sub(r"[^\w\- ]+", "", s, flags=re.UNICODE)
    s = s.strip().replace(" ", "-")
    return s


def doc_link(base_url, heading):
    """文档章节级链接。"""
    slug = gh_slug(heading)
    return f"{base_url}#{slug}" if slug else base_url


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
    doc_base = ("https://github.com/earendil-works/pi/blob/main/"
                "packages/coding-agent/docs/settings.md")
    for heading, header, rows in parse_tables_by_section(md):
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
            # 官方文档没有逐键链接，但能定位到「配置项属于哪一节」
            if heading:
                node["x-doc-url"] = doc_link(doc_base, heading)
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
    doc_base = ("https://github.com/xai-org/grok-build/blob/main/crates/codegen/"
                "xai-grok-pager/docs/user-guide/26-config-reference.md")
    for heading, header, rows in parse_tables_by_section(md):
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
            # 章节级锚点：官方文档是按小节写的，定位到小节比没有链接强得多
            if heading:
                node["x-doc-url"] = doc_link(doc_base, heading)
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

    # 官方生成式文档里的字段说明 + 章节级链接
    descs, links = openclaw_doc_info()
    applied = 0
    for path, desc in descs.items():
        node = lookup(schema, path)
        if node is not None:
            set_desc(node, desc)
            applied += 1
    # 把文档链接贴到 schema 节点上（父路径的链接覆盖整棵子树）
    n_links = 0

    def apply_links(node, prefix, inherited):
        nonlocal n_links
        props = node.get("properties") if isinstance(node, dict) else None
        if not props:
            return
        for k, v in props.items():
            if not isinstance(v, dict):
                continue
            path = f"{prefix}{k}"
            link = links.get(path) or inherited
            if link and "x-doc-url" not in v:
                v["x-doc-url"] = link
                n_links += 1
            apply_links(v, path + ".", link)

    apply_links(schema, "", "")
    n = count_leaves(schema)
    return schema, (f"openclaw: {n} 个叶子键，{applied} 条来自官方文档的说明，"
                   f"{n_links} 个键带文档链接")


def openclaw_doc_pages():
    base = "https://raw.githubusercontent.com/openclaw/openclaw/main/docs/gateway/"
    return [base + p for p in (
        "config-runtime.md", "config-extensions.md", "config-browser-ui-desktop.md",
        "config-gateway.md", "config-cloud-workers.md", "config-hooks.md",
        "config-secrets-env.md", "config-observability.md", "config-automation.md",
        "config-agents.md", "config-channels.md", "config-tools.md",
    )]


def strip_json5(text):
    """把 json5 片段清理成合法 JSON。

    处理：行注释 / 块注释、裸键、单引号、尾逗号。
    注释要在字符串之外才去掉（"https://..." 不能被截断）。
    """
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c in "\"'":
            q = c
            out.append(c)
            i += 1
            while i < n:
                if text[i] == "\\" and i + 1 < n:
                    out.append(text[i:i + 2])
                    i += 2
                    continue
                out.append(text[i])
                if text[i] == q:
                    i += 1
                    break
                i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            i += 2
            while i + 1 < n and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
            continue
        out.append(c)
        i += 1
    t = "".join(out)
    # 裸键加引号
    t = re.sub(r"([{,]\s*)([A-Za-z_$][\w$]*)(\s*:)", r'\1"\2"\3', t)
    # 单引号字符串
    t = re.sub(r"'([^'\\\n]*)'", r'"\1"', t)
    # 尾逗号
    t = re.sub(r",(\s*[}\]])", r"\1", t)
    return t


def json5_paths(text):
    """解析 json5，返回 {点分路径: 值}。解析不了就返回空。"""
    try:
        obj = json.loads(strip_json5(text))
    except Exception:  # noqa: BLE001
        return {}
    out = {}

    def walk(node, prefix):
        if isinstance(node, dict):
            for k, v in node.items():
                path = f"{prefix}{k}"
                out[path] = v
                walk(v, path + ".")
        elif isinstance(node, list):
            for v in node:
                walk(v, prefix)

    walk(obj, "")
    return out


def code_blocks(md_text):
    """抽出所有代码块：[(起始行, 结束行, 内容)]。"""
    lines = md_text.splitlines()
    out = []
    i = 0
    while i < len(lines):
        if lines[i].strip().startswith("```"):
            lang = lines[i].strip()[3:].strip().lower()
            j = i + 1
            body = []
            while j < len(lines) and not lines[j].strip().startswith("```"):
                body.append(lines[j])
                j += 1
            out.append((i, j, lang, "\n".join(body)))
            i = j + 1
        else:
            i += 1
    return out


def openclaw_doc_info():
    """从官方生成式文档里抽 (路径 → 说明) 与 (路径 → 文档链接)。

    文档结构固定：
        ## `worktreeRoot`
        Optional global root directory for ... Defaults to `<openclaw-state-dir>/worktrees`.
        - `enabled`: enables TLS termination ... (default: `false`).

    所以有两种来源：
      1) 标题（`## \\`path\\``）后**第一段散文** = 该字段自己的说明
      2) 标题下的 `- \\`leaf\\`: 说明` = 子字段的说明

    顺带记下每个路径的文档链接（章节级锚点）。
    """
    descs = {}
    links = {}
    for url in openclaw_doc_pages():
        page = url.rsplit("/", 1)[-1]
        base = f"https://github.com/openclaw/openclaw/blob/main/docs/gateway/{page}"
        md = fetch_text(url)
        if not md:
            continue
        cur = ""
        lines = md.splitlines()
        # 每个小节前面的 json5 代码块给出配置树 → 用来定位 bullet 的完整路径
        blocks = [(s, e, json5_paths(b)) for (s, e, lang, b) in code_blocks(md)
                  if lang in ("json5", "json", "") and b.strip().startswith("{")]
        for i, line in enumerate(lines):
            hm = re.match(r"^#{1,4}\s+`([a-zA-Z0-9_.\[\]*]+)`\s*$", line)
            if hm:
                cur = hm.group(1)
                links.setdefault(cur, doc_link(base, cur))
                # 标题后的第一段散文就是该字段的说明
                desc = first_paragraph(lines, i + 1)
                if desc:
                    descs.setdefault(cur, desc)
                continue
            # 不是路径的标题 → 清掉当前前缀。
            # 不清的话，下面的 bullet 会挂到上一个路径头上（错位）。
            if re.match(r"^#{1,4}\s+", line):
                cur = ""
                continue
            bm = re.match(r"^\s*[-*]\s+`([^`]+)`\s*[:：]\s*(.+)$", line)
            if bm:
                raw, desc = bm.group(1), strip_md(bm.group(2))
                if not desc:
                    continue
                # 反引号里可能是 `path: "value"`（路径 + 取值）→ 只留路径
                leaf = raw
                if ":" in leaf:
                    leaf = leaf.split(":")[0].strip()
                if not leaf:
                    continue
                if "." in leaf or "*" in leaf:
                    # bullet 本身就是完整路径（可能带 `*` 通配）
                    out_path = leaf
                elif cur:
                    out_path = f"{cur}.{leaf}"
                else:
                    # 非路径小节：靠最近的 json5 代码块定位完整路径
                    cand = None
                    for (bs, be, paths) in blocks:
                        if be <= i and paths:
                            cand = paths
                    if not cand:
                        continue
                    hits = [p for p in cand if p.split(".")[-1] == leaf]
                    if not hits:
                        continue
                    # 同名有多个时取最浅的那个（更可能是主键）
                    out_path = sorted(hits, key=lambda p: p.count("."))[0]
                descs.setdefault(out_path, desc)
                links.setdefault(out_path, links.get(cur) or base)
    return descs, links


def openclaw_doc_descriptions():
    """兼容旧调用：只要说明部分。"""
    descs, _links = openclaw_doc_info()
    return descs


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
        ap = node.get("additionalProperties")
        if isinstance(ap, dict):
            walk(ap)
        t = node.get("type")
        if t in ("object", "array"):
            return
        if t or "enum" in node or "const" in node:
            n += 1

    for v in schema.get("properties", {}).values():
        walk(v)
    return n


# ── minimax-code：官方 npm 包里的配置定义 ─────────────────

# ── minimax-code：官方仓库的 TypeScript 配置定义 ───────

MM_REPO_RAW = ("https://raw.githubusercontent.com/MiniMax-AI/minimax-code/main/"
                "packages/config/src/config.ts")
MM_REPO_BLOB = ("https://github.com/MiniMax-AI/minimax-code/blob/main/"
                "packages/config/src/config.ts")
# 指向仓库里任意文件的 blob 前缀（拼接相对路径用）
MM_BLOB_BASE = "https://github.com/MiniMax-AI/minimax-code/blob/main/"


def parse_ts_interfaces(src, file_label=""):
    """解析 TS interface → {接口名: [(字段, JSDoc, 类型, 行号, 文件)]}。"""
    out = {}
    for m in re.finditer(r"^(?:export\s+)?interface\s+(\w+)\s*\{", src, re.M):
        name = m.group(1)
        start = m.end()
        depth = 1
        i = start
        while i < len(src) and depth > 0:
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
            i += 1
        body = src[start:i - 1]
        base_line = src[:start].count("\n") + 1
        fields = []
        doc = []
        in_doc = False
        for off, ln in enumerate(body.split("\n")):
            s = ln.strip()
            if s.startswith("/**"):
                in_doc = True
                doc = []
                continue
            if in_doc:
                if s.startswith("*/"):
                    in_doc = False
                    continue
                if s.startswith("*"):
                    t = s[1:].strip()
                    if t:
                        doc.append(t)
                    continue
            if not s:
                if not in_doc:
                    doc = []
                continue
            if s.startswith("//") or s.startswith("*"):
                continue
            fm = re.match(r"^([A-Za-z_]\w*)\??\s*:\s*(.+?);$", s)
            if fm:
                fields.append((fm.group(1), " ".join(doc), fm.group(2),
                              base_line + off, file_label))
                doc = []
                continue
            doc = []
        out[name] = fields
    return out


def parse_ts_extends(src):
    """解析 interface 的 extends 关系 → {接口名: [基接口名]}。"""
    out = {}
    for m in re.finditer(r"^(?:export\s+)?interface\s+(\w+)\s+extends\s+([^\{]+)\{", src, re.M):
        bases = [b.strip() for b in m.group(2).split(",") if b.strip()]
        out[m.group(1)] = bases
    return out


def parse_ts_aliases(src):
    """解析 type 别名 → {别名: 类型表达式}。

    例：`export type ModelsConfig = Record<string, ProviderConfig>;`
    """
    out = {}
    for m in re.finditer(r"^(?:export\s+)?type\s+(\w+)\s*=\s*(.+?);\s*$", src, re.M):
        out[m.group(1)] = m.group(2).strip()
    return out


def ts_children(typ, ifaces, aliases, extends_map, depth=0):
    """类型表达式 → [(键名, JSDoc, 子类型表达式, 行号, 文件)]。

    支持：
      - `Record<string, X>`      → [("*", X, "", 0, "")]（动态键）
      - 裸接口名 `X`            → X 的字段（extends 的基类字段在前）
      - type 别名 `X`           → 递归解析别名
      - 内联对象字面量 `{ a: string; b?: number }`
    """
    if depth > 6:
        return []
    t = (typ or "").strip().rstrip(";").strip()
    t = re.sub(r"^\s*readonly\s+", "", t)
    t = re.sub(r"\s*\|\s*undefined$", "", t)
    t = re.sub(r"^\s*undefined\s*\|\s*", "", t)
    if not t:
        return []

    m = re.fullmatch(r"Record<\s*string\s*,\s*(.+)>", t)
    if m:
        return [("*", "", m.group(1).strip(), 0, "")]

    m = re.fullmatch(r"([A-Z]\w*)", t)
    if m:
        name = m.group(1)
        if name in ifaces:
            fields = []
            for base in extends_map.get(name, []):
                if base in ifaces:
                    fields.extend(ifaces[base])
            fields.extend(ifaces[name])
            return fields
        if name in aliases:
            return ts_children(aliases[name], ifaces, aliases, extends_map, depth + 1)
        return []

    # 内联对象字面量
    if t.startswith("{") and t.endswith("}"):
        body = t[1:-1]
        out = []
        for part in body.split(";"):
            part = part.strip()
            if not part:
                continue
            fm = re.match(r"^([A-Za-z_]\w*)\??\s*:\s*(.+)$", part)
            if fm:
                out.append((fm.group(1), "", fm.group(2).strip(), 0, ""))
        return out
    return []



def ts_type_to_schema(typ):
    """TS 类型 → JSON Schema 片段。"""
    t = typ.strip().rstrip(";").strip()
    t = re.sub(r"^\s*readonly\s+", "", t)
    t = re.sub(r"\s*\| undefined$", "", t)
    t = re.sub(r"^\s*undefined\s*\|", "", t)
    if t.startswith("'") and t.endswith("'"):
        return {"type": "string"}
    # 联合字面量 → 枚举
    if "|" in t and all(re.fullmatch(r'"[^"]*"', p.strip()) for p in t.split("|")):
        vals = [p.strip().strip('"') for p in t.split("|")]
        return {"type": "string", "enum": vals}
    if t in ("string", "String"):
        return {"type": "string"}
    if t in ("number", "Number"):
        return {"type": "number"}
    if t in ("boolean", "Boolean"):
        return {"type": "boolean"}
    if re.fullmatch(r"string\[\]", t):
        return {"type": "array", "items": {"type": "string"}}
    if re.fullmatch(r"number\[\]", t):
        return {"type": "array", "items": {"type": "number"}}
    if re.fullmatch(r"boolean\[\]", t):
        return {"type": "array", "items": {"type": "boolean"}}
    if re.fullmatch(r"Array<(.+)>", t):
        inner = ts_type_to_schema(re.match(r"Array<(.+)>", t).group(1))
        return {"type": "array", "items": inner}
    m = re.fullmatch(r"Record<\s*string\s*,\s*(.+)>", t)
    if m:
        return {"type": "object", "additionalProperties": ts_type_to_schema(m.group(1))}
    m = re.fullmatch(r"\{(.+)\}", t)
    if m:
        props = {}
        for part in m.group(1).split(";"):
            fm = re.match(r"\s*([A-Za-z_]\w*)\??\s*:\s*(.+)", part)
            if fm:
                props[fm.group(1)] = ts_type_to_schema(fm.group(2))
        return {"type": "object", "properties": props}
    if re.fullmatch(r"[A-Z]\w*", t):
        return {"type": "object"}
    return {}


# ── gemini-cli：官方文档逐键说明（补充层） ───────────

GEMINI_DOC_RAW = ("https://raw.githubusercontent.com/google-gemini/gemini-cli/main/"
                  "docs/reference/configuration.md")
GEMINI_DOC_BLOB = ("https://github.com/google-gemini/gemini-cli/blob/main/"
                   "docs/reference/configuration.md")


def doc_section_mentions(md, base_url, key_re=r"`([A-Za-z_][\w]*(?:\.[\w]+)*)`"):
    """扫文档各小节，把小节里**提到的键**记到该小节的锚点上。

    有些工具的文档是叙述式的（没有逐键小节），但正文里会提到键名。
    把「提到它的那个小节」作为链接，比完全没有链接有用得多。
    """
    out = {}
    cur = ""
    for line in md.splitlines():
        hm = re.match(r"^#{2,4}\s+(.+?)\s*$", line)
        if hm:
            cur = strip_md(hm.group(1))
            continue
        if not cur:
            continue
        for k in re.findall(key_re, line):
            if k not in out:
                out[k] = doc_link(base_url, cur)
    return out


def doc_layer(md, base_url):
    """把「小节里提到的键」整理成补充层的标准格式。

    标准格式与 gemini-cli 的补充层一致：
        `{ "键路径": { "description": "...", "doc_url": "..." } }`
    这里只有 doc_url —— 说明文字要靠官方散文原句，不做二次加工。
    """
    mentions = doc_section_mentions(md, base_url)
    out = {}
    for k, u in mentions.items():
        out[k] = {"doc_url": u}
    return out


# ── schema 感知的文档层 ────────────────────────────────
#
# 上面的「小节提及」是盲扫：抓到什么算���么。
# 这一套反过来做 —— **以 schema 里的真实键为准**，去官方文档里找
# 「哪个小节讲到了它」，找不到就留空（前端再降级到「搜文档」）。

def load_tool_schema(tool_id):
    """拿工具的 schema：优先仓库 cache/ 里的官方抓取结果，其次 schemas/<id>.json。"""
    cands = []
    cache = os.path.join(ROOT, "cache")
    if os.path.isdir(cache):
        for fn in sorted(os.listdir(cache)):
            if fn.startswith(tool_id + "-") and fn.endswith(".json"):
                cands.append(os.path.join(cache, fn))
    cands.append(os.path.join(SCHEMA_DIR, f"{tool_id}.json"))
    for p in cands:
        try:
            with open(p, encoding="utf-8") as f:
                return json.load(f)
        except Exception:  # noqa: BLE001
            continue
    return None


def schema_paths(schema):
    """schema 里的全部路径（含中间层），`[*]` 归一化成 `*`。

    动态键（`additionalProperties` 是对象）也展开，动态段用 `*` 表示。
    """
    out = []

    def walk(node, prefix):
        if not isinstance(node, dict):
            return
        props = node.get("properties")
        if isinstance(props, dict):
            for k, v in props.items():
                p = f"{prefix}{k}"
                out.append(p)
                walk(v, p + ".")
        add = node.get("additionalProperties")
        if isinstance(add, dict) and add:
            p = f"{prefix}*"
            out.append(p)
            walk(add, p + ".")

    walk(schema, "")
    return out


def _norm_key(k):
    """把键名归一：`[*]` → `*`，去掉首尾噪声。"""
    k = k.strip().strip("`\"'")
    k = k.replace("[*]", "*").replace("[]", "*")
    return k


def section_tokens(lines):
    """抽出一个小节里出现的所有「键形态 token」。

    来源有三：行内代码 `` `foo.bar` ``、JSON/JSON5 代码块的键路径、
    YAML 代码块的键路径。都按点分路径收集。
    """
    toks = set()
    for line in lines:
        for m in re.finditer(r"`([^`\n]+)`", line):
            t = m.group(1).strip()
            # `` `a` / `b` `` 这种枚举写法，拆开
            for piece in re.split(r"[,\s]+", t):
                piece = piece.strip("`\"'")
                if not piece or " " in piece:
                    continue
                if not re.fullmatch(r"[\w.\*\[\]-]+", piece):
                    continue
                if piece.lower() in ("true", "false", "null", "none", "yes", "no"):
                    continue
                toks.add(_norm_key(piece))
    # 代码块
    text = "\n".join(lines)
    for _s, _e, lang, content in code_blocks(text):
        paths = json5_paths(content)
        if not paths and lang in ("yaml", "yml"):
            try:
                import yaml  # noqa: PLC0415

                obj = yaml.safe_load(content)
                paths = {}

                def walk_yaml(node, prefix):
                    if isinstance(node, dict):
                        for k, v in node.items():
                            p = f"{prefix}{k}"
                            paths[p] = v
                            walk_yaml(v, p + ".")
                    elif isinstance(node, list):
                        for v in node:
                            walk_yaml(v, prefix)

                walk_yaml(obj, "")
            except Exception:  # noqa: BLE001
                pass
        for p in paths:
            toks.add(_norm_key(p))
            # 同时把每段后缀都收进来，便于逐级回退匹配
            parts = p.split(".")
            for i in range(1, len(parts)):
                toks.add(_norm_key(".".join(parts[i:])))
    return toks


def doc_sections(md, base_url):
    """把文档按小节切开，每个小节带上标题、锚点 URL 和 token 集合。"""
    sections = []
    cur = None
    for line in md.splitlines():
        hm = re.match(r"^(#{2,6})\s+(.+?)\s*$", line)
        if hm:
            raw = hm.group(2)
            cur = {
                "heading": strip_md(raw),
                "url": doc_link(base_url, raw),
                "lines": [],
            }
            sections.append(cur)
            continue
        if cur is not None:
            cur["lines"].append(line)
    for s in sections:
        s["tokens"] = section_tokens(s["lines"])
    return sections


def derive_doc_layer(tool_id, md, base_url):
    """以 schema 的真实键为准，给每个键找到「官方文档里讲它的那一节」。

    返回 `{ "键路径": { "doc_url": "章节锚点" } }`。
    找不到对应小节的键不出现 —— 前端会对它们走「搜文档」降级。
    """
    return derive_doc_layer_multi(tool_id, [(md, base_url)])


def derive_doc_layer_multi(tool_id, docs):
    """多个文档页合并成一层（OpenClaw 的配置文档分散在 12 个 md 文件里）。

    docs: [(md_text, base_url)]
    """
    schema = load_tool_schema(tool_id)
    if schema is None:
        return None, f"找不到 {tool_id} 的 schema"

    sections = []
    for md, base_url in docs:
        sections.extend(doc_sections(md, base_url))
    if not sections:
        return None, "文档里没解析出小节"

    paths = schema_paths(schema)
    out = {}
    # 先按 token 建索引：token → 第一个提到它的小节
    token_index = {}
    for s in sections:
        for t in s["tokens"]:
            token_index.setdefault(t, s)

    for p in paths:
        parts = p.split(".")
        # 候选：完整路径 → 逐级去掉前缀（providers[*].models[*].name → …）
        cands = []
        for i in range(len(parts)):
            cands.append(".".join(parts[i:]))
        for c in cands:
            hit = token_index.get(c)
            if hit is not None:
                out[p] = {"doc_url": hit["url"]}
                break
    return out, f"{tool_id}: {len(out)}/{len(paths)} 个 schema 路径定位到文档小节"



def write_supplement(name, data):
    path = os.path.join(SCHEMA_DIR, name)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1, sort_keys=True)
    return path


def derive_opencode_doc():
    """OpenCode 官方配置文档 → 文档补充层。"""
    url = ("https://raw.githubusercontent.com/anomalyco/opencode/dev/"
           "packages/web/src/content/docs/config.mdx")
    blob = ("https://github.com/anomalyco/opencode/blob/dev/"
            "packages/web/src/content/docs/config.mdx")
    md = fetch_text(url)
    if md is None:
        return None, "抓不到 opencode 的 config.mdx"
    out, note = derive_doc_layer("opencode", md, blob)
    if out is None:
        return None, note
    path = write_supplement("opencode-doc.json", out)
    return out, f"{note} → {os.path.relpath(path, ROOT)}"


def derive_openclaw_doc():
    """OpenClaw 的 12 个官方配置文档页 → 文档补充层。

    OpenClaw 的 schema 有 1500+ 个叶子键，但官方文档只在散文里提到一部分。
    这里按「哪个小节提到了这个键」反查，给每个键挂上章节级锚点。
    """
    docs = []
    for url in openclaw_doc_pages():
        page = url.rsplit("/", 1)[-1]
        blob = f"https://github.com/openclaw/openclaw/blob/main/docs/gateway/{page}"
        md = fetch_text(url)
        if md:
            docs.append((md, blob))
    if not docs:
        return None, "openclaw 的配置文档一个都没抓到"
    out, note = derive_doc_layer_multi("openclaw", docs)
    if out is None:
        return None, note
    path = write_supplement("openclaw-doc.json", out)
    return out, f"{note}（{len(docs)} 个文档页）→ {os.path.relpath(path, ROOT)}"


def derive_hermes_doc():
    """Hermes 官方配置指南 → 文档补充层。"""
    url = ("https://raw.githubusercontent.com/NousResearch/hermes-agent/main/"
           "website/docs/user-guide/configuration.md")
    blob = ("https://github.com/NousResearch/hermes-agent/blob/main/"
            "website/docs/user-guide/configuration.md")
    md = fetch_text(url)
    if md is None:
        return None, "抓不到 hermes 的 configuration.md"
    out, note = derive_doc_layer("hermes", md, blob)
    if out is None:
        return None, note
    path = write_supplement("hermes-doc.json", out)
    return out, f"{note} → {os.path.relpath(path, ROOT)}"


def derive_gemini_cli_doc():
    """Gemini CLI 官方配置文档 → 文档补充层。

    文档结构：
        #### `general`
        - **`general.preferredEditor`** (enum):
          - **Description:** The preferred editor to open files in. ...
          - **Default:** `undefined`
          - **Values:** `"vscode"`, `"vscodium"`, ...

    官方 schema 只给结构，这个文档才有逐键的文字说明。
    """
    md = fetch_text(GEMINI_DOC_RAW)
    if md is None:
        return None, "抓不到 gemini-cli 的配置文档"

    out = {}
    section = ""
    lines = md.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        hm = re.match(r"^#{2,4}\s+`([^`]+)`\s*$", line)
        if hm:
            section = hm.group(1)
            i += 1
            continue
        # - **`path`** (type):
        km = re.match(r"^\s*-\s+\*\*`([^`]+)`\*\*\s*(\([^)]*\))?\s*:?\s*$", line)
        if km:
            key = km.group(1)
            desc_parts = []
            value_parts = []
            default = None
            values = None
            mode = "desc"
            j = i + 1
            while j < len(lines):
                s = lines[j].strip()
                if re.match(r"^\s*-\s+\*\*`", s) or re.match(r"^#{2,4}\s", s):
                    break
                dm = re.match(r"^\s*-\s+\*\*Description:\*\*\s*(.*)$", s)
                if dm:
                    desc_parts.append(dm.group(1).strip())
                    mode = "desc"
                    j += 1
                    continue
                dd = re.match(r"^\s*-\s+\*\*Default:\*\*\s*(.*)$", s)
                if dd:
                    default = parse_default(dd.group(1).strip())
                    mode = "default"
                    j += 1
                    continue
                dv = re.match(r"^\s*-\s+\*\*Values:\*\*\s*(.*)$", s)
                if dv:
                    values = dv.group(1)
                    value_parts = [values]
                    mode = "values"
                    j += 1
                    continue
                if s and not s.startswith("-"):
                    # 续行属于当前块（说明 / 候选值都可能折行）
                    if mode == "desc":
                        desc_parts.append(s)
                    elif mode == "values":
                        value_parts.append(s)
                j += 1
            desc = " ".join(p for p in desc_parts if p).strip()
            # 续行是上一行的自然延续，去掉换行造成的多余空格
            desc = re.sub(r"\s+", " ", desc)
            entry = {}
            if desc:
                entry["description"] = desc
            if default is not None:
                entry["default"] = default
            if value_parts:
                joined = " ".join(value_parts)
                vals = [v.strip().strip('"`') for v in joined.split(",")]
                vals = [v for v in vals if v]
                if len(vals) > 1:
                    entry["values"] = vals
            if section:
                entry["doc_url"] = doc_link(GEMINI_DOC_BLOB, section)
            if entry:
                out[key] = entry
            i = j
            continue
        i += 1
    return out, f"gemini-cli: {len(out)} 个键的文档说明"


def derive_gemini_cli_doc_file():
    """生成 schemas/gemini-cli-doc.json。"""
    data, note = derive_gemini_cli_doc()
    if data is None:
        return None, note
    path = os.path.join(SCHEMA_DIR, "gemini-cli-doc.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1, sort_keys=True)
    return data, note + f" → {os.path.relpath(path, ROOT)}"


def fetch_minimax_sources():
    """拉官方仓库 `packages/config/src` 下的全部 .ts（权威定义）。

    不只是 config.ts：Config 接口引用的一堆子接口（PermissionConfig、
    AgentsConfig、AsrConfig、SkillsConfig、TuiConfig…）都定义在兄弟文件里，
    字段说明也只在那些文件里。
    """
    tree_url = ("https://api.github.com/repos/MiniMax-AI/minimax-code/"
                "git/trees/main?recursive=1")
    raw = fetch_text(tree_url)
    files = []
    if raw:
        try:
            tree = json.loads(raw)
        except Exception:  # noqa: BLE001
            tree = {}
        for t in tree.get("tree", []):
            p = t.get("path", "")
            if t.get("type") != "blob":
                continue
            if not p.startswith("packages/config/src/") or not p.endswith(".ts"):
                continue
            if "/test" in p or p.endswith(".test.ts"):
                continue
            files.append(p)
    srcs = {}
    for p in sorted(files):
        txt = fetch_text(
            "https://raw.githubusercontent.com/MiniMax-AI/minimax-code/main/" + p)
        if txt:
            srcs[p] = txt
    if not srcs:
        src = fetch_text(MM_REPO_RAW)
        if src:
            srcs["packages/config/src/config.ts"] = src
    return srcs


def derive_minimax_code():
    """官方仓库的 TypeScript 配置定义 → JSON Schema。

    官方 npm 包里只有压缩后的默认值，而仓库 `packages/config/src/` 下的
    TypeScript 是权威定义：字段上方还有 JSDoc 说明 —— 这就是官方给的字段文档。
    文档链接精确到「字段定义所在的那一行」。
    """
    srcs = fetch_minimax_sources()
    if not srcs:
        return None, "抓不到 minimax-code 的源码"

    ifaces = {}
    extends_map = {}
    aliases = {}
    for path, txt in srcs.items():
        for name, fields in parse_ts_interfaces(txt, path).items():
            ifaces.setdefault(name, fields)
        for name, bases in parse_ts_extends(txt).items():
            extends_map.setdefault(name, bases)
        for name, expr in parse_ts_aliases(txt).items():
            aliases.setdefault(name, expr)

    if "Config" not in ifaces:
        return None, "源码里找不到 Config 接口"

    n_desc = [0]

    def build(typ, depth=0):
        """类型表达式 → schema 节点（含子键、说明、精确到行的链接）。"""
        node = ts_type_to_schema(typ)
        if depth > 6:
            return node
        kids = ts_children(typ, ifaces, aliases, extends_map, depth)
        if not kids:
            return node
        props = {}
        for fld, doc, ktyp, line, fpath in kids:
            cn = build(ktyp, depth + 1)
            if doc:
                cn.setdefault("description", doc)
                n_desc[0] += 1
            if line and fpath:
                cn["x-doc-url"] = f"{MM_BLOB_BASE}{fpath}#L{line}"
            props[fld] = cn
        node["type"] = "object"
        if list(props) == ["*"]:
            node.pop("properties", None)
            node["additionalProperties"] = props["*"]
        else:
            node["properties"] = props
        return node

    schema = build("Config")
    if not isinstance(schema.get("properties"), dict) or not schema["properties"]:
        return None, "Config 接口没解析出字段"

    # 官方 npm 包里的默认值（结构更准，能补上 default）
    defaults = minimax_defaults_from_package()
    if defaults:
        def merge_def(node, prefix):
            props = node.get("properties")
            if not props:
                return
            for k, v in props.items():
                if isinstance(v, dict) and "properties" in v:
                    merge_def(v, f"{prefix}{k}.")
                elif prefix + k in defaults:
                    v.setdefault("default", defaults[prefix + k])

        merge_def(schema, "")

    n = count_leaves(schema)
    return schema, (f"minimax-code: {n} 个叶子键，{n_desc[0]} 条来自官方源码的说明"
                    f"（{len(srcs)} 个源码文件）")


def minimax_defaults_from_package():
    """从官方 npm 包里抠出配置默认值（压缩过的 JS 对象字面量）。"""
    pkg = "/tmp/mmx/package"
    if not os.path.isdir(pkg):
        return {}
    out = {}
    for root, _dirs, files in os.walk(os.path.join(pkg, "chunks")):
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
            if not lit:
                continue
            obj = parse_js_object(lit)
            if not obj:
                continue
            for k, v in obj.items():
                out[k] = v
            return out
    return out


def derive_minimax_code_legacy():
    """旧版：只从官方 npm 包抠默认值（保留作对照）。"""
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

def fetch_text(url, timeout=40, retries=3, cache=True):
    """抓文本；GitHub raw 偶尔限流，重试几次。

    带本地缓存 —— 派生脚本要抓十几个文档页，重复调试时别每次都重新拉。
    """
    key = None
    if cache:
        import hashlib
        key = hashlib.sha256(url.encode("utf-8")).hexdigest()[:16]
        cpath = os.path.join("/tmp/cfg-derive-cache", key + ".txt")
        if os.path.exists(cpath):
            try:
                with open(cpath, encoding="utf-8") as f:
                    return f.read()
            except OSError:
                pass
    for attempt in range(retries):
        try:
            r = subprocess.run(
                ["curl", "-s", "--compressed", "-m", str(timeout), "-A",
                 "cfg-explorer/0.1 (schema deriver)", url],
                capture_output=True, timeout=timeout + 10)
            if r.returncode == 0 and r.stdout:
                text = r.stdout.decode("utf-8", errors="ignore")
                if cache and key:
                    try:
                        os.makedirs("/tmp/cfg-derive-cache", exist_ok=True)
                        with open(os.path.join("/tmp/cfg-derive-cache", key + ".txt"),
                                  "w", encoding="utf-8") as f:
                            f.write(text)
                    except OSError:
                        pass
                return text
        except Exception:  # noqa: BLE001
            pass
        time.sleep(2 * (attempt + 1))
    return None


# ── 主流程 ────────────────────────────────────────────────

DERIVERS = {
    "pi": derive_pi,
    "gemini-cli-doc": derive_gemini_cli_doc_file,
    "opencode-doc": derive_opencode_doc,
    "hermes-doc": derive_hermes_doc,
    "openclaw-doc": derive_openclaw_doc,
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
        # 补充层产物不是 schema（它已经自己写盘了），只报个数
        if name.endswith("-doc"):
            print(f"  ✓ {name}: {note}")
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
