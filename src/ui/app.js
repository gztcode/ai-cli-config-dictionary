// AI CLI 配置字典 · 渲染层
//
// 三个页面（顶栏导航切换）：
//   1. 配置字典 —— 浏览 schema 里的全部 key、看说明与文档链接、校验本机配置
//   2. 扫描磁盘 —— 找出本机所有项目与其中的 AI CLI 配置文件
//   3. 编辑器   —— 独立的编辑子页面，改完可校验、可预览差异、可安全写回
//
// 两种数据来源：
//   1) 桌面 App：window.__TAURI__.core.invoke(...)
//   2) 静态 HTML 导出：页面内嵌 <script id="payload"> JSON
//
// 两种来源共用同一份渲染逻辑，UI 不会漂移。

let DATA = null;

// 当前页面：'dict' | 'scan' | 'edit'
let PAGE = 'dict';

let cur = 0, lang = 'zh-CN', q = '', filt = 'all';

// ── 扫描 ──────────────────────────────────────────────
let SCAN = {
  options: { roots: [], filename: '', content: '', known_only: true, limit: 500, max_depth: 8 },
  result: null,
  running: false,
  scope: 'home',
  onlyProjects: true,   // 只显示带 AI CLI 配置的项目
  toolFilter: '',       // 按工具筛选
};

// ── 编辑器 ────────────────────────────────────────────
let EDITOR = {
  file: null,
  ops: [],
  undo: [],          // 撤销栈：每次改动前的 ops 快照
  redo: [],          // 重做栈
  newText: null,
  diff: [],
  dirty: false,
  validation: null,
  stale: false,
  recent: [],        // 最近打开的文件
  revealed: {},      // 已展开明文的路径 -> 明文
  panel: null,       // 辅助面板：'add' | 'newfile' | null
  addCtx: null,      // 新增键的上下文：{ parent: [], kind: 'key' | 'array' }
  addMode: 'schema', // 新增键面板的来源：'schema' | 'custom'
  newFile: null,     // 新建文件面板的状态
};

const isDesktop = () => !!(window.__TAURI__ && window.__TAURI__.core);
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

// 真的桌面 App（不是 HTTP 桥接的垫片）—— 两者都有 __TAURI__.core
const isDesktopApp = () => isDesktop() && !window.__TAURI_SHIM__;

const el = (t, cls, txt) => {
  const e = document.createElement(t);
  if (cls) e.className = cls;
  if (txt != null) e.textContent = txt;
  return e;
};

// ── 外链 ────────────────────────────────────────────────

// 说明文字里经常内嵌官方文档地址（「参见 https://code.claude.com/docs/...」），
// 之前是纯文本 —— 看得见却点不动。这里把它们变成真链接。
const URL_RE = /\bhttps?:\/\/[^\s<>"'）】，。；：、]+/g;

/** 生成一个指向外部地址的链接 */
function externalLink(url, text, cls) {
  const a = el('a', cls || null, text || url);
  a.href = url;
  a.target = '_blank';
  a.rel = 'noreferrer noopener';
  a.onclick = (e) => {
    e.stopPropagation();
    // 桌面 App 里交给系统默认浏览器；网页版让浏览器自己处理
    if (isDesktopApp()) {
      e.preventDefault();
      openExternal(url);
    }
  };
  return a;
}

/** 把文本里的 URL 变成可点击的链接（其余仍是纯文本） */
function linkify(text) {
  const frag = document.createDocumentFragment();
  if (text == null || text === '') return frag;
  const s = String(text);
  let last = 0;
  URL_RE.lastIndex = 0;
  let m;
  while ((m = URL_RE.exec(s)) !== null) {
    // 去掉结尾可能粘上的标点
    let url = m[0];
    while (url.length && '.,;:!?'.includes(url[url.length - 1])) url = url.slice(0, -1);
    if (!url) continue;
    const at = m.index;
    if (at > last) frag.appendChild(document.createTextNode(s.slice(last, at)));
    frag.appendChild(externalLink(url));
    last = at + url.length;
  }
  if (last < s.length) frag.appendChild(document.createTextNode(s.slice(last)));
  return frag;
}

/** 填一段可能含链接的文字（替代 textContent） */
function setText(node, text) {
  node.textContent = '';
  node.appendChild(linkify(text));
  return node;
}

function getPath(root, path) {
  if (!path) return undefined;
  let c = root;
  for (const seg of path.split('.')) {
    if (seg === '[]' || seg === '[*]') continue;
    if (c == null || typeof c !== 'object') return undefined;
    c = c[seg];
  }
  return c;
}

function fmt(v) {
  if (v === undefined) return null;
  if (v === null) return 'null';
  return JSON.stringify(v);
}

function segs(path) {
  return path.split('.').map((s, i, a) =>
    i === a.length - 1 ? `<span class="seg">${s}</span>` : `<span class="seg-l">${s}</span>.`
  ).join('');
}

function normPath(p) {
  return String(p).replace(/\[\d+\]/g, '').replace(/\.\[\*?\]?$/, '').replace(/\.$/, '');
}

function buildIssueIndex(reports) {
  const direct = {}, errSet = new Set(), warnSet = new Set();
  for (const r of reports) for (const is of r.issues) {
    const k = normPath(is.path);
    (direct[k] = direct[k] || []).push(is);
    if (is.level === 'error') errSet.add(k); else warnSet.add(k);
  }
  return { direct, errSet, warnSet };
}

function subtreeHas(key, set) {
  if (set.has(key.path)) return true;
  for (const c of (key.children || [])) if (subtreeHas(c, set)) return true;
  return false;
}

/// 这个键本身（或它的某个后代）在本机配置里有没有值
function subtreeConfigured(key, config) {
  if (getPath(config, key.path) !== undefined) return true;
  for (const c of (key.children || [])) if (subtreeConfigured(c, config)) return true;
  return false;
}

/// 是不是容器（有子键）
function isObjRow(key) {
  return !!(key.children && key.children.length && key.kind !== 'leaf');
}

// ── 顶栏导航 ──────────────────────────────────────────

const PAGES = [
  ['dict', '配置字典'],
  ['scan', '扫描磁盘'],
  ['edit', '编辑器'],
];

function renderNav() {
  let nav = document.getElementById('nav');
  if (!nav) {
    nav = el('nav');
    nav.id = 'nav';
    const header = document.querySelector('header');
    // 放在「刷新 schema」之前；没有它就放在语言按钮之前
    const anchor = document.getElementById('refresh') || document.getElementById('lang');
    header.insertBefore(nav, anchor);
  }
  nav.innerHTML = '';
  for (const [p, label] of PAGES) {
    if (p === 'scan' && !isDesktop()) continue;   // 静态导出没有 IPC
    const b = el('button', PAGE === p ? 'on' : '', label);
    b.onclick = () => { PAGE = p; render(); };
    nav.appendChild(b);
  }
}

// ── 页面 1：配置字典 ──────────────────────────────────

function renderDictSide() {
  const side = document.getElementById('side');
  if (!DATA) return;
  DATA.tools.forEach((t, i) => {
    const errs = t.reports.reduce((n, r) => n + r.issues.filter(x => x.level === 'error').length, 0);
    const warns = t.reports.reduce((n, r) => n + r.issues.filter(x => x.level === 'warning').length, 0);
    const d = errs ? 'err' : (warns ? 'warn' : (t.schema_available ? 'ok' : 'none'));
    const box = el('div', 'tool' + (i === cur ? ' on' : ''));
    const n = el('div', 'n');
    n.appendChild(el('span', null, t.display_name));
    const dt = el('span', 'dot ' + d);
    dt.title = errs ? `${errs} 个错误` : (warns ? `${warns} 个警告` : '无问题');
    n.appendChild(dt);
    box.appendChild(n);
    box.appendChild(el('div', 's', `${t.stats.total_keys} keys · ${t.stats.with_doc_url} doc`));
    box.onclick = () => { cur = i; PAGE = 'dict'; render(); };
    side.appendChild(box);
  });
}

function renderRow(key, tool, idx) {
  const wrap = el('div', isObjRow(key) ? 'rowobj' : 'rowleaf');
  const isObj = isObjRow(key);
  const hdr = el('div', 'row hdr');
  hdr.appendChild(el('div', 'tw', isObj ? '▾' : '·'));

  const p = el('div', 'path');
  p.innerHTML = segs(key.path);
  hdr.appendChild(p);

  const vt = key.value_type;
  hdr.appendChild(el('span', 'tag t-' + (vt === 'enum' ? 'enum' : vt), vt));
  if (key.required) hdr.appendChild(el('span', 'tag req', '必填'));
  if (key.deprecated) hdr.appendChild(el('span', 'tag dep', '废弃'));
  if (key.doc_urls && key.doc_urls.length) hdr.appendChild(el('span', 'tag doc', 'DOC'));
  if (key.translations && Object.keys(key.translations).length) hdr.appendChild(el('span', 'tag i18n', 'i18n'));

  // 本机是否已配置
  const curVal = getPath(tool.config, key.path);
  const isSet = curVal !== undefined;
  if (isSet) hdr.appendChild(el('span', 'tag set', '已配置'));

  // 说明文字：按 fallback 链「请求语言 → 覆盖层 → 英文原文」择一显示。
  // 全量中文之后每条都有译文，英文原文不再并列刷屏，改为可点开对照。
  const desc = el('div', 'desc');
  const tr = key.translations && key.translations[lang];
  const en = key.description || '';
  if (lang === 'zh-CN' && tr) {
    // 说明里的 URL 要能直接点开
    desc.appendChild(setText(el('span', 'zh'), tr));
    if (en && en !== tr) {
      const toggle = el('a', 'enorig', '原文');
      toggle.href = '#';
      toggle.onclick = (e) => {
        e.stopPropagation();
        const old = desc.querySelector('.enfull');
        if (old) { old.remove(); toggle.textContent = '原文'; }
        else { desc.appendChild(setText(el('div', 'enfull'), en)); toggle.textContent = '收起'; }
      };
      desc.appendChild(document.createTextNode(' '));
      desc.appendChild(toggle);
    }
  } else {
    setText(desc, en || (key.doc_urls && key.doc_urls.length ? '' : '（无官方说明）'));
  }
  hdr.appendChild(desc);
  wrap.appendChild(hdr);

  const kids = el('div', 'kids');
  if (isSet) {
    const c = el('div', 'cur click');
    let s = fmt(curVal);
    const LIMIT = 240;
    if (s.length > LIMIT) s = s.slice(0, LIMIT) + ` … (共 ${s.length} 字符)`;
    c.textContent = '当前值: ' + s;
    c.title = fmt(curVal);
    // 点当前值 → 进编辑器打开该工具的配置文件
    const target = (tool.reports || []).find(r => r.exists);
    if (target) {
      c.title = fmt(curVal) + '\n点击在编辑器中打开 ' + target.path;
      c.onclick = () => openFile(target.path);
    }
    kids.appendChild(c);
  }
  if (key.doc_urls && key.doc_urls.length) {
    const l = el('div', 'links');
    key.doc_urls.forEach(u => l.appendChild(externalLink(u)));
    kids.appendChild(l);
  }
  for (const is of (idx.direct[key.path] || [])) {
    const d = el('div', 'iss ' + (is.level === 'error' ? 'e' : 'w'));
    d.textContent = (is.level === 'error' ? '✕ ' : '⚠ ') + is.path + ' — ' + is.message;
    kids.appendChild(d);
  }
  if (kids.childNodes.length) wrap.appendChild(kids);

  if (isObj) {
    const box = el('div', 'kids');
    key.children.forEach(c => {
      if (c.name === '[]' || c.name === '[*]') return;
      if (q && !((c.path + ' ' + (c.description || '')).toLowerCase().includes(q.toLowerCase()))) return;
      if (filt === 'set' && !subtreeConfigured(c, tool.config)) return;
      box.appendChild(renderRow(c, tool, idx));
    });
    if (box.childNodes.length) wrap.appendChild(box);
  }
  return wrap;
}

function renderDict(main) {
  const t = DATA.tools[cur];
  if (!t) return;

  const s = t.stats;
  const stats = el('div', 'stats');
  const chip = (l, v) => {
    const c = el('span', 'chip');
    c.appendChild(document.createTextNode(l + ' '));
    c.appendChild(el('b', null, v));
    stats.appendChild(c);
  };
  chip('总键数', s.total_keys); chip('叶子键', s.leaf_keys);
  chip('叶子有说明', s.leaf_documented + '/' + s.leaf_total);
  chip('叶子带文档链接', s.leaf_with_doc_url + '/' + s.leaf_total);
  chip('有枚举', s.with_enum); chip('有默认值', s.with_default);
  chip('已废弃', s.deprecated); chip('已翻译', s.translated);
  main.appendChild(stats);

  if (!t.schema_available) {
    // 无公开 schema：说清楚为什么空，并给出「文档入口 + 配置文件直达」
    const b = el('div', 'src');
    b.textContent = '⚠ ' + t.schema_note;
    main.appendChild(b);

    const hint = el('div', 'tipbox');
    hint.appendChild(el('div', 'tiphead', '这个工具能做什么'));
    hint.appendChild(el('div', 'tipline',
      '它的配置项没有公开的机器可读 schema，所以字典里列不出键位。'));
    hint.appendChild(el('div', 'tipline',
      '下面的路径是它在本机的已知配置位置——直接去编辑器里看实际内容，比猜键名靠谱。'));
    if (t.docs_url) {
      const ln = el('div', 'tipline');
      ln.appendChild(document.createTextNode('官方文档：'));
      ln.appendChild(externalLink(t.docs_url));
      hint.appendChild(ln);
    }
    main.appendChild(hint);
  } else if (t.source) {
    const b = el('div', 'src');
    b.textContent = 'schema: ';
    const url = t.source.url || '';
    if (url.startsWith('builtin:')) {
      // 内置的派生 schema 没有远程地址，别做成能点的假链接
      b.appendChild(el('span', 'srcfile', url.replace(/^builtin:/, '')));
    } else {
      b.appendChild(externalLink(url));
    }
    b.appendChild(document.createTextNode(
      `  · sha256 ${t.source.sha256.slice(0, 16)}…  · ${t.source.bytes} B  · ${t.source.fetched_at}`));
    main.appendChild(b);
  }

  // 本机配置文件：每条都能直接进编辑器
  for (const r of t.reports) {
    const b = el('div', 'src');
    b.textContent = r.exists ? `· ${r.path}  (${r.key_count} 个值)` : `· ${r.path}  (不存在)`;
    if (r.error) b.textContent += '  解析失败: ' + r.error;
    if (r.exists) {
      const ed = el('a', null, '编辑');
      ed.href = '#';
      ed.onclick = (e) => { e.preventDefault(); openFile(r.path); };
      b.appendChild(document.createTextNode('  '));
      b.appendChild(ed);
      if (isDesktop()) {
        const a = el('a', null, '在访达中显示');
        a.href = '#';
        a.onclick = (e) => {
          e.preventDefault();
          invoke('reveal_path', { path: r.path }).catch(() => {});
        };
        b.appendChild(document.createTextNode('  '));
        b.appendChild(a);
      }
    }
    main.appendChild(b);
  }

  const bar = el('div', 'bar');
  const inp = el('input');
  inp.type = 'search'; inp.placeholder = '搜索 key 或描述…'; inp.value = q;
  inp.oninput = e => { q = e.target.value; render(); };
  bar.appendChild(inp);
  ['all', 'error', 'warning', 'nodoc', 'deprecated', 'set'].forEach(f => {
    const labels = { all: '全部', error: '有错误', warning: '有警告', nodoc: '无文档链接', deprecated: '已废弃', set: '本机已配置' };
    const b = el('button', filt === f ? 'on' : '', labels[f]);
    b.onclick = () => { filt = f; render(); };
    bar.appendChild(b);
  });
  main.appendChild(bar);

  const idx = buildIssueIndex(t.reports);
  let shown = 0;
  for (const k of t.keys) {
    const hasErr = subtreeHas(k, idx.errSet), hasWarn = subtreeHas(k, idx.warnSet);
    if (filt === 'error' && !hasErr) continue;
    if (filt === 'warning' && !hasWarn) continue;
    if (filt === 'nodoc' && (k.doc_urls && k.doc_urls.length)) continue;
    if (filt === 'deprecated' && !k.deprecated) continue;
    if (filt === 'set' && !subtreeConfigured(k, t.config)) continue;
    if (q && !((k.path + ' ' + (k.description || '')).toLowerCase().includes(q.toLowerCase()))) continue;
    main.appendChild(renderRow(k, t, idx));
    shown++;
  }
  if (!shown) main.appendChild(el('div', 'empty', '没有匹配的键'));
}

// ── 页面 2：扫描磁盘 ──────────────────────────────────

const SCAN_SCOPES = [
  ['home', '用户目录'],
  ['projects', '常见项目目录'],
  ['disk', '全盘'],
  ['custom', '自定义路径'],
];

function renderScanSide() {
  const side = document.getElementById('side');
  const box = el('div', 'scanbox');

  const row = el('div', 'scanrow');
  row.appendChild(el('label', null, '范围'));
  const sel = el('select');
  for (const [v, t] of SCAN_SCOPES) {
    const o = el('option', null, t); o.value = v;
    if (SCAN.scope === v) o.selected = true;
    sel.appendChild(o);
  }
  sel.onchange = () => { SCAN.scope = sel.value; render(); };
  row.appendChild(sel);
  box.appendChild(row);

  if (SCAN.scope === 'custom') {
    const r2 = el('div', 'scanrow');
    r2.appendChild(el('label', null, '路径'));
    const inp = el('input');
    inp.type = 'text'; inp.placeholder = '/Users/you/Projects';
    inp.value = (SCAN.options.roots || []).join(',');
    inp.oninput = e => { SCAN.options.roots = e.target.value.split(',').map(s => s.trim()).filter(Boolean); };
    r2.appendChild(inp);
    box.appendChild(r2);
  }

  const r3 = el('div', 'scanrow');
  r3.appendChild(el('label', null, '文件名'));
  const f = el('input');
  f.type = 'text'; f.placeholder = 'settings.json';
  f.value = SCAN.options.filename;
  f.oninput = e => { SCAN.options.filename = e.target.value; };
  r3.appendChild(f);
  box.appendChild(r3);

  const r4 = el('div', 'scanrow');
  r4.appendChild(el('label', null, '内容'));
  const c = el('input');
  c.type = 'text'; c.placeholder = 'ANTHROPIC_BASE_URL';
  c.value = SCAN.options.content;
  c.oninput = e => { SCAN.options.content = e.target.value; };
  r4.appendChild(c);
  box.appendChild(r4);

  const r5 = el('div', 'scanrow check');
  const cb = el('input'); cb.type = 'checkbox'; cb.checked = SCAN.options.known_only;
  cb.onchange = e => { SCAN.options.known_only = e.target.checked; };
  r5.appendChild(cb);
  r5.appendChild(el('label', null, '只匹配已知配置文件名'));
  box.appendChild(r5);

  const go = el('button', 'primary', SCAN.running ? '扫描中…' : '开始扫描');
  go.disabled = SCAN.running;
  go.onclick = () => startScan();
  box.appendChild(go);

  side.appendChild(box);
}

async function scanRoots() {
  if (SCAN.scope === 'custom') return SCAN.options.roots || [];
  if (!isDesktop()) return [];
  try {
    const roots = await invoke('scan_roots', { scope: SCAN.scope });
    return roots || [];
  } catch (e) {
    return SCAN.scope === 'disk' ? ['/'] : ['~'];
  }
}

async function startScan() {
  if (!isDesktop()) { alert('扫描功能仅在桌面应用中可用'); return; }
  const roots = await scanRoots();
  if (!roots.length) { alert('没有可扫描的路径'); return; }
  SCAN.running = true;
  render();
  try {
    const opts = {
      roots: roots,
      filename: SCAN.options.filename || '',
      content: SCAN.options.content || '',
      known_only: SCAN.options.known_only,
      max_size: 2 * 1024 * 1024,
      limit: SCAN.options.limit || 500,
      max_depth: SCAN.scope === 'disk' ? 14 : 12,
      detect_projects: true,
      max_projects: 5000,
    };
    SCAN.result = await invoke('scan_files', { options: opts });
  } catch (e) {
    SCAN.result = { files: [], projects: [], error: String(e) };
  }
  SCAN.running = false;
  render();
}

function fileRow(f) {
  const row = el('div', 'filerow');
  const top = el('div', 'ft');
  const nm = el('div', 'fname');
  nm.appendChild(el('span', 'tag ' + (f.format === 'toml' ? 't-num' : f.format === 'yaml' ? 't-bool' : 't-str'), f.format));
  const fpath = el('span', 'fpath');
  // 用 bdi 隔离方向：.fpath 用 direction:rtl 让省略号落在开头，
  // 但路径本身必须保持 LTR 顺序，否则开头的 "/" 会被甩到末尾
  const bdi = document.createElement('bdi');
  bdi.dir = 'ltr';
  bdi.textContent = f.path;
  fpath.appendChild(bdi);
  fpath.title = f.path;
  nm.appendChild(fpath);
  top.appendChild(nm);
  if (f.tool) top.appendChild(el('span', 'tag t-obj', f.tool));
  row.appendChild(top);

  const meta = el('div', 'fmeta');
  meta.textContent = (f.size < 1024 ? `${f.size} B` : `${(f.size / 1024).toFixed(1)} KB`) +
    (f.modified ? ` · ${f.modified}` : '') +
    (f.matched_by === 'content' ? ' · 内容命中' : '');
  row.appendChild(meta);

  const actions = el('div', 'fact');
  const open = el('button', 'primary', '编辑');
  open.onclick = () => openFile(f.path);
  actions.appendChild(open);
  if (isDesktop()) {
    const rv = el('button', null, '访达中显示');
    rv.onclick = () => invoke('reveal_path', { path: f.path }).catch(() => {});
    actions.appendChild(rv);
  }
  row.appendChild(actions);
  return row;
}

function renderScan(main) {
  const info = el('div', 'src');
  if (SCAN.running) {
    info.textContent = '正在遍历磁盘，请稍候…';
  } else if (!SCAN.result) {
    info.textContent = '在左侧设置范围与关键字，点「开始扫描」。';
  } else if (SCAN.result.error) {
    info.textContent = '扫描失败：' + SCAN.result.error;
  } else {
    const r = SCAN.result;
    const projCount = (r.projects || []).length;
    info.textContent = `找到 ${r.files.length} 个配置文件 · ${projCount} 个项目 · 用时 ${(r.elapsed_ms / 1000).toFixed(1)}s` +
      (r.truncated ? ' · 文件结果已达上限' : '');
  }
  main.appendChild(info);

  if (!SCAN.result || !SCAN.result.files) return;

  // 概览 + 项目/全局切换 + 按工具筛选
  const bar = el('div', 'bar');
  const toggle = el('button', SCAN.onlyProjects ? 'on' : '', '只看带配置的项目');
  toggle.onclick = () => { SCAN.onlyProjects = !SCAN.onlyProjects; render(); };
  bar.appendChild(toggle);

  // 按工具筛选
  const tools = [];
  for (const f of (SCAN.result.files || [])) {
    const t = f.tool || '（未识别）';
    if (!tools.includes(t)) tools.push(t);
  }
  tools.sort();
  if (tools.length > 1) {
    const all = el('button', !SCAN.toolFilter ? 'on' : '', '全部工具');
    all.onclick = () => { SCAN.toolFilter = ''; render(); };
    bar.appendChild(all);
    for (const t of tools) {
      const b = el('button', SCAN.toolFilter === t ? 'on' : '', t);
      b.onclick = () => { SCAN.toolFilter = SCAN.toolFilter === t ? '' : t; render(); };
      bar.appendChild(b);
    }
  }
  main.appendChild(bar);

  const projects = SCAN.result.projects || [];
  const allFiles = (SCAN.result.files || []).filter(f => !SCAN.toolFilter || (f.tool || '（未识别）') === SCAN.toolFilter);
  const files = allFiles;
  const byProject = new Map();
  const globals = [];
  for (const f of files) {
    if (f.project) {
      if (!byProject.has(f.project)) byProject.set(f.project, []);
      byProject.get(f.project).push(f);
    } else {
      globals.push(f);
    }
  }

  // 项目分组
  let shownProjects = 0;
  for (const p of projects) {
    const list = byProject.get(p.root) || [];
    if (SCAN.onlyProjects && list.length === 0) continue;
    shownProjects++;
    const sec = el('div', 'projsec');
    const h = el('div', 'projhead');
    h.appendChild(el('span', 'tag t-obj', p.name));
    h.appendChild(el('span', 'projroot', p.root));
    h.appendChild(el('span', 'projcount', list.length ? `${list.length} 个配置` : '暂无配置'));
    sec.appendChild(h);
    if (p.markers && p.markers.length) {
      sec.appendChild(el('div', 'projmarkers', '标志：' + p.markers.join('、')));
    }
    for (const f of list) sec.appendChild(fileRow(f));
    main.appendChild(sec);
  }
  if (shownProjects) main.appendChild(el('div', 'empty', '— 以上为项目内的配置 —'));

  // 不属于任何项目的（全局配置）
  if (globals.length) {
    const sec = el('div', 'projsec');
    const h = el('div', 'projhead');
    h.appendChild(el('span', 'tag t-obj', '全局配置'));
    h.appendChild(el('span', 'projroot', '不属于任何项目（用户级配置）'));
    h.appendChild(el('span', 'projcount', `${globals.length} 个`));
    sec.appendChild(h);
    for (const f of globals) sec.appendChild(fileRow(f));
    main.appendChild(sec);
  }

  if (!files.length) main.appendChild(el('div', 'empty', '没有找到匹配的文件'));
}

// ── 页面 3：编辑器（独立子页面）───────────────────────

function renderEditSide() {
  const side = document.getElementById('side');

  if (!isDesktop()) {
    const tip = el('div', 'tipbox');
    tip.appendChild(el('div', 'tiphead', '静态预览模式'));
    tip.appendChild(el('div', 'tipline', '编辑需要桌面应用（写文件要经过系统接口）。'));
    tip.appendChild(el('div', 'tipline', '用桌面版 AI 配置字典即可扫描并编辑本机配置。'));
    side.appendChild(tip);
    return;
  }

  // 当前文件
  if (EDITOR.file) {
    const box = el('div', 'curfile');
    box.appendChild(el('div', 'cftitle', '当前文件'));
    box.appendChild(el('div', 'cfpath', EDITOR.file.path));
    const acts = el('div', 'cfacts');
    const reload = el('button', null, '重新载入');
    reload.onclick = () => openFile(EDITOR.file.path);
    acts.appendChild(reload);
    if (isDesktop()) {
      const rv = el('button', null, '访达中显示');
      rv.onclick = () => invoke('reveal_path', { path: EDITOR.file.path }).catch(() => {});
      acts.appendChild(rv);
    }
    box.appendChild(acts);
    side.appendChild(box);
  }

  // 打开其它文件
  const box = el('div', 'scanbox');
  const r = el('div', 'scanrow');
  r.appendChild(el('label', null, '打开文件'));
  const inp = el('input');
  inp.type = 'text';
  inp.placeholder = '输入配置文件的完整路径';
  inp.onkeydown = e => { if (e.key === 'Enter') openFile(inp.value.trim()); };
  r.appendChild(inp);
  box.appendChild(r);
  const go = el('button', 'primary', '打开');
  go.onclick = () => openFile(inp.value.trim());
  box.appendChild(go);

  const nf = el('button', null, '新建配置文件…');
  nf.id = 'btn-newfile';
  nf.onclick = () => { EDITOR.panel = 'newfile'; PAGE = 'edit'; render(); };
  box.appendChild(nf);
  side.appendChild(box);

  // 最近打开
  if (EDITOR.recent.length) {
    const rlist = el('div', 'recent');
    rlist.appendChild(el('div', 'cftitle', '最近打开'));
    for (const p of EDITOR.recent) {
      const it = el('div', 'ritem');
      it.appendChild(el('span', 'rpath', p));
      it.title = p;
      it.onclick = () => openFile(p);
      rlist.appendChild(it);
    }
    side.appendChild(rlist);
  }

  if (!EDITOR.file) {
    const tip = el('div', 'tipbox');
    tip.appendChild(el('div', 'tiphead', '怎么用'));
    tip.appendChild(el('div', 'tipline', '① 到「扫描磁盘」页挑一个项目里的配置文件'));
    tip.appendChild(el('div', 'tipline', '② 或在上方直接输入路径打开'));
    tip.appendChild(el('div', 'tipline', '③ 改完点「校验」，再点「保存到文件」'));
    side.appendChild(tip);
  }
}

async function openFile(path) {
  if (!path) return;
  if (!isDesktop()) { alert('编辑功能仅在桌面应用中可用'); return; }
  try {
    EDITOR.file = await invoke('open_file', { path });
    EDITOR.ops = [];
    EDITOR.undo = [];
    EDITOR.redo = [];
    EDITOR.revealed = {};
    EXPANDED = new Set();
    EDITOR.newText = null;
    EDITOR.diff = [];
    EDITOR.dirty = false;
    EDITOR.validation = null;
    EDITOR.stale = false;
    EDITOR.recent = [path, ...EDITOR.recent.filter(p => p !== path)].slice(0, 12);
    PAGE = 'edit';
    render();
    // 打开后先按 schema 校验一遍当前状态
    await runValidation();
    // 校验完才知道这个文件属于哪个工具，树的说明文字要靠它，
    // 所以补一次重绘（此刻还没有输入焦点，重绘是安全的）
    render();
  } catch (e) {
    alert('打开失败：' + e);
  }
}

/// 跑校验（不重绘整页，只刷新校验面板，避免打断正在输入的输入框）
async function runValidation() {
  const f = EDITOR.file;
  if (!f) return;
  try {
    EDITOR.validation = await invoke('validate_edit', { path: f.path, ops: EDITOR.ops });
  } catch (e) {
    EDITOR.validation = {
      tool: null, schema_available: false, issues: [],
      error_count: 0, warn_count: 0, parse_error: String(e),
    };
  }
  EDITOR.stale = false;
  renderValidationPanel();
}

function renderValidationPanel() {
  const box = document.getElementById('valbox');
  if (!box) return;
  box.innerHTML = '';
  const v = EDITOR.validation;
  if (!v) return;

  const head = el('div', 'valhead');
  const tool = v.tool ? v.tool : '未识别工具';
  head.appendChild(el('span', 'tag t-obj', tool));
  if (!v.schema_available && !v.parse_error) {
    head.appendChild(el('span', 'valnote', '该工具暂无公开 schema，只能做格式校验'));
  } else {
    head.appendChild(el('span', 'valnote',
      `${v.error_count} 个错误 · ${v.warn_count} 个警告`));
  }
  if (EDITOR.stale) head.appendChild(el('span', 'valnote warn', '内容已改动，需重新校验'));
  box.appendChild(head);

  if (v.parse_error) {
    box.appendChild(el('div', 'iss e', '✕ 解析失败 — ' + v.parse_error));
    return;
  }
  for (const is of (v.issues || [])) {
    box.appendChild(el('div', 'iss ' + (is.level === 'error' ? 'e' : 'w'),
      (is.level === 'error' ? '✕ ' : '⚠ ') + is.path + ' — ' + is.message));
  }
}

function valueToInput(v) {
  if (v === null || v === undefined) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  return JSON.stringify(v);
}

function inputToValue(text, old) {
  if (typeof old === 'boolean') return text === 'true' || text === '1';
  if (typeof old === 'number') {
    const n = Number(text);
    return Number.isNaN(n) ? text : n;
  }
  if (typeof old !== 'string' && text !== '') {
    try { return JSON.parse(text); } catch (e) { return text; }
  }
  return text;
}

/// 同步编辑器按钮的可用状态与计数（输入时按钮不能一直是灰的）
function refreshEditorState() {
  const has = EDITOR.ops.length > 0;
  for (const id of ['btn-preview', 'btn-save', 'btn-revert']) {
    const b = document.getElementById(id);
    if (b) b.disabled = !has;
  }
  const st = document.querySelector('.edstatus');
  if (st) st.textContent = has ? `${EDITOR.ops.length} 处未保存修改` : '';
}

/// 记录一条编辑操作，并刷新按钮状态
function pushOp(op, opts) {
  // 撤销栈：先存一份改动前的快照
  EDITOR.undo.push(EDITOR.ops.map(o => ({ ...o })));
  if (EDITOR.undo.length > 100) EDITOR.undo.shift();
  EDITOR.redo = [];

  const key = JSON.stringify(op.path);
  EDITOR.ops = EDITOR.ops.filter(o => {
    // 位置型操作（插入 / 移动）不能合并：它们的 path 指向数组本身，
    // 同一数组上的多次操作必须按顺序全部保留。
    if (o.op === 'insert' || o.op === 'move') return true;
    if (JSON.stringify(o.path) !== key) return true;
    // 同一个 key 只保留最后一次操作
    return o.op !== op.op;
  });
  EDITOR.ops.push(op);
  EDITOR.dirty = true;
  EDITOR.stale = true;
  if (opts && opts.rerender) {
    render();
  } else {
    refreshEditorState();
    renderValidationPanel();
  }
}

/// 撤销一步
function undoOp() {
  if (!EDITOR.undo.length) return;
  EDITOR.redo.push(EDITOR.ops.map(o => ({ ...o })));
  EDITOR.ops = EDITOR.undo.pop();
  EDITOR.dirty = EDITOR.ops.length > 0;
  EDITOR.stale = true;
  refreshEditorState();
  render();
}

/// 重做一步
function redoOp() {
  if (!EDITOR.redo.length) return;
  EDITOR.undo.push(EDITOR.ops.map(o => ({ ...o })));
  EDITOR.ops = EDITOR.redo.pop();
  EDITOR.dirty = EDITOR.ops.length > 0;
  EDITOR.stale = true;
  refreshEditorState();
  render();
}

/// 路径 → 点分字符串（数组下标保留）
const pathKey = (path) => path.join('.');

/// 这个路径是不是密钥字段（后端已经脱敏，这里只判断要不要显示「显示明文」）
function isSecretPath(path) {
  const set = new Set(EDITOR.file && EDITOR.file.secret_paths || []);
  if (set.size) return set.has(pathKey(path));
  // 后端没给清单时，退回按路径名判断
  return path.some(seg => /api[_-]?key|token|secret|password|credential|auth/i.test(seg));
}

// ── 通用弹窗 ──────────────────────────────────────────

/// 多选一弹窗，返回被选项的 id（取消返回 null）
function askChoice(title, message, choices) {
  return new Promise(resolve => {
    const back = el('div', 'modalback');
    const box = el('div', 'modal');
    box.appendChild(el('div', 'modaltitle', title));
    if (message) box.appendChild(el('div', 'modalmsg', message));
    for (const c of choices) {
      const b = el('button', 'modalbtn');
      b.appendChild(el('span', 'ml', c.label));
      if (c.desc) b.appendChild(el('span', 'md', c.desc));
      b.onclick = () => { back.remove(); resolve(c.id); };
      box.appendChild(b);
    }
    const cancel = el('button', 'modalcancel', '取消');
    cancel.onclick = () => { back.remove(); resolve(null); };
    box.appendChild(cancel);
    back.appendChild(box);
    back.onclick = e => { if (e.target === back) { back.remove(); resolve(null); } };
    document.body.appendChild(back);
  });
}

// ── schema 索引：把工具的 IR 按 path 建索引，供编辑器查说明 ─────────

/// toolId -> Map<path, IrKey>
const SCHEMA_INDEX = new Map();

function schemaIndexOf(tool) {
  if (!tool) return new Map();
  if (SCHEMA_INDEX.has(tool.id)) return SCHEMA_INDEX.get(tool.id);
  const m = new Map();
  const walk = (keys) => {
    for (const k of keys || []) {
      m.set(k.path, k);
      walk(k.children);
    }
  };
  walk(tool.keys || []);
  SCHEMA_INDEX.set(tool.id, m);
  return m;
}

/// 当前打开的文件属于哪个工具
///
/// 优先用「工具的配置文件清单」匹配路径（打开即生效），
/// 匹配不到再退回校验结果里的工具名。
function toolForFile(path) {
  if (!DATA || !path) return null;
  for (const t of DATA.tools) {
    for (const r of t.reports || []) {
      if (r.path === path) return t;
    }
  }
  return currentTool();
}

/// 查某个键在 schema 里的说明信息
///
/// 路径是编辑器的分段数组（数组元素是数字下标），schema 里
/// 数组元素用 `xxx[]` 表示，所以要逐级回退匹配。
function schemaInfoFor(tool, segs) {
  const idx = schemaIndexOf(tool);
  if (!idx.size || !segs || !segs.length) return null;
  const isNum = (s) => /^\d+$/.test(s);
  // 匹配顺序：完整路径 → 数组元素（xxx[]）→ 数组本身（xxx）→ 父级
  // 「元素」必须排在「数组」前面，否则数组元素的说明会错拿到数组的说明，
  // 类型也会从 string 变成 array。
  const tryPaths = [segs.join('.')];
  let cur = segs.slice();
  while (cur.length > 1) {
    const removed = cur[cur.length - 1];
    cur = cur.slice(0, -1);
    const base = cur.join('.');
    if (isNum(removed)) tryPaths.push(base + '[]');
    tryPaths.push(base);
  }
  for (const p of tryPaths) {
    const hit = idx.get(p);
    if (hit) return hit;
  }
  return null;
}

/// 取一条说明的中文译文（没有译文就用英文原文）
function descOf(info) {
  if (!info) return '';
  const zh = info.translations && info.translations['zh-CN'];
  return zh || info.description || '';
}

// ── 新增键 / 添加数组元素 ──────────────────────────────

/// 某个工具的 schema 里所有可选的叶子键
///
/// 同时带上中文译文、默认值、枚举候选与说明——新增键时都要展示出来。
function schemaKeyOptions(tool) {
  const out = [];
  const walk = (keys) => {
    for (const k of keys || []) {
      if (k.name === '[]' || k.name === '[*]') continue;
      if (/\[|\]/.test(k.path)) continue;      // 跳过数组通配路径
      const leaf = k.kind === 'leaf' || !(k.children || []).length;
      if (leaf) {
        const zh = (k.translations && k.translations['zh-CN']) || '';
        out.push({
          path: k.path,
          type: k.value_type,
          desc: zh || k.description || '',       // 优先中文
          descZh: zh,
          descEn: k.description || '',
          def: k.default,
          enum: k.enum_values || [],
          deprecated: k.deprecated,
          required: k.required,
          doc: (k.doc_urls || [])[0] || '',
        });
      }
      walk(k.children);
    }
  };
  walk(tool.keys || []);
  return out;
}

/// 枚举候选选择器：可筛选的列表
///
/// schema 里带枚举的键有几十个候选（如 permissions.defaultMode），
/// 直接铺开没法看，所以给个搜索框 + 列表，点一下选中。
function enumPicker(enumValues, onPick, def) {
  const wrap = el('div', 'enumpick');
  const search = el('input');
  search.type = 'search';
  search.placeholder = `筛选候选值（${enumValues.length} 个）…`;
  wrap.appendChild(search);

  const items = el('div', 'enumitems');
  wrap.appendChild(items);

  const draw = (kw) => {
    items.innerHTML = '';
    const k = kw.trim().toLowerCase();
    let shown = 0;
    for (const ev of enumValues) {
      const val = fmt(ev.value);
      const d = ev.description || '';
      if (k && !((val + ' ' + d).toLowerCase().includes(k))) continue;
      if (shown++ > 200) break;
      const it = el('div', 'enumitem');
      it.appendChild(el('span', 'ev', val));
      if (d) it.appendChild(setText(el('span', 'ed'), d));
      // schema 里的默认值标出来
      if (def !== undefined && def !== null && fmt(def) === val) {
        it.appendChild(el('span', 'tag t-str', '默认'));
      }
      it.onclick = () => {
        for (const x of items.querySelectorAll('.enumitem')) x.classList.remove('psel');
        it.classList.add('psel');
        onPick(ev.value);
      };
      items.appendChild(it);
    }
    if (!shown) items.appendChild(el('div', 'empty', '没有匹配的候选值'));
  };
  search.oninput = () => draw(search.value);
  draw('');
  return wrap;
}

/// 按类型给一个零值
function zeroValue(type, def, enums) {
  if (def !== undefined && def !== null) return def;
  if (enums && enums.length) return enums[0];
  switch (type) {
    case 'number': case 'integer': return 0;
    case 'boolean': return false;
    case 'array': return [];
    case 'object': return {};
    default: return '';
  }
}

/// 字符串 → 猜测类型
function guessType(text) {
  if (text === 'true' || text === 'false') return 'boolean';
  if (text !== '' && !Number.isNaN(Number(text))) return 'number';
  if (/^[[{]/.test(text.trim())) {
    try { const v = JSON.parse(text); return Array.isArray(v) ? 'array' : 'object'; }
    catch (e) { return 'string'; }
  }
  return 'string';
}

/// 从字符串构造值（尽量符合 schema 类型）
function coerce(text, type) {
  if (type === 'boolean') return text === 'true' || text === '1';
  if (type === 'number' || type === 'integer') {
    const n = Number(text);
    return Number.isNaN(n) ? text : n;
  }
  if (type === 'array' || type === 'object') {
    try { return JSON.parse(text); } catch (e) { return text; }
  }
  return text;
}

function renderAddPanel(main) {
  const ctx = EDITOR.addCtx || { parent: [], kind: 'key' };
  const box = el('div', 'panel');
  const head = el('div', 'panelhead');
  head.appendChild(el('span', null, ctx.kind === 'array' ? '添加数组元素' : '新增配置键'));
  const close = el('button', 'mini', '关闭');
  close.onclick = () => { EDITOR.panel = null; render(); };
  head.appendChild(close);
  box.appendChild(head);

  const target = ctx.parent && ctx.parent.length ? ctx.parent.join('.') : '（根）';
  box.appendChild(el('div', 'paneltip', `目标位置：${target}`));

  if (ctx.kind === 'array') {
    // 数组元素：类型 + 值
    // 先说说这个数组是干什么的、元素该长什么样（schema 里是 `xxx[]`）
    const atool = toolForFile(EDITOR.file && EDITOR.file.path);
    const einfo = schemaInfoFor(atool, (ctx.parent || []).concat('0'));
    const edesc = descOf(einfo);
    if (edesc) {
      const d = el('div', 'pdesc');
      d.appendChild(setText(el('span', 'zh'), edesc));
      const ein = einfo && einfo.translations && einfo.translations['zh-CN'];
      if (einfo && einfo.description && einfo.description !== ein) {
        const tg = el('a', 'enorig', '原文');
        tg.href = '#';
        tg.onclick = (e) => {
          e.preventDefault();
          const old = d.querySelector('.enfull');
          if (old) { old.remove(); tg.textContent = '原文'; }
          else { d.appendChild(setText(el('div', 'enfull'), einfo.description)); tg.textContent = '收起'; }
        };
        d.appendChild(document.createTextNode(' '));
        d.appendChild(tg);
      }
      box.appendChild(d);
    }
    // 元素类型有 schema 的话，直接给成下拉
    const etype = einfo && einfo.value_type;
    const row = el('div', 'panelrow');
    row.appendChild(el('label', null, '类型'));
    const sel = el('select');
    const types = etype && etype !== 'any'
      ? [etype].concat(['string', 'number', 'boolean', 'object', 'array'].filter(t => t !== etype))
      : ['string', 'number', 'boolean', 'object', 'array'];
    for (const t of types) {
      const o = el('option', null, t); o.value = t; sel.appendChild(o);
    }
    row.appendChild(sel);
    box.appendChild(row);

    const row2 = el('div', 'panelrow');
    row2.appendChild(el('label', null, '值'));
    const inp = el('input');
    inp.type = 'text';
    const hasDef = einfo && einfo.default !== undefined && einfo.default !== null;
    inp.placeholder = hasDef
      ? `默认值：${fmt(einfo.default)}`
      : '留空则用类型零值；对象/数组可填 JSON';
    row2.appendChild(inp);
    box.appendChild(row2);

    const go = el('button', 'primary', '添加到数组末尾');
    go.onclick = () => {
      const text = inp.value.trim();
      const value = text === ''
        ? zeroValue(sel.value, hasDef ? einfo.default : undefined, einfo && einfo.enum_values)
        : coerce(text, sel.value);
      pushOp({ op: 'insert', path: ctx.parent, index: 0x7fffffff, value }, { rerender: true });
      EDITOR.panel = null;
      render();
    };
    box.appendChild(go);
    main.appendChild(box);
    return;
  }

  // ── 新增键：两种来源 ──
  const tabs = el('div', 'paneltabs');
  const mode = EDITOR.addMode || 'schema';
  const tabSchema = el('button', mode === 'schema' ? 'on' : '', '从 schema 选');
  tabSchema.onclick = () => { EDITOR.addMode = 'schema'; render(); };
  const tabCustom = el('button', mode === 'custom' ? 'on' : '', '自由输入');
  tabCustom.onclick = () => { EDITOR.addMode = 'custom'; render(); };
  tabs.appendChild(tabSchema);
  tabs.appendChild(tabCustom);
  box.appendChild(tabs);

  if (mode === 'schema') {
    const tool = toolForFile(EDITOR.file && EDITOR.file.path);
    if (!tool) {
      box.appendChild(el('div', 'paneltip', '当前文件不属于已注册的工具，改用「自由输入」。'));
      main.appendChild(box);
      return;
    }
    if (!tool.schema_available) {
      box.appendChild(el('div', 'paneltip', `${tool.display_name} 暂无公开 schema，只能自由输入键名。`));
      main.appendChild(box);
      return;
    }
    const opts = schemaKeyOptions(tool);
    const list = el('div', 'panelopts');
    const search = el('input');
    search.type = 'search';
    search.placeholder = '搜索键名或说明（中英文均可）…';
    list.appendChild(search);

    // 选中的键：详情 + 取值区。
    // 放在列表**上方** —— 候选可能有几百条，选完看不到详情就没意义了。
    const detail = el('div', 'pdetail');
    detail.style.display = 'none';
    list.appendChild(detail);

    const items = el('div', 'panelitems');
    list.appendChild(items);

    const draw = (kw) => {
      items.innerHTML = '';
      const k = kw.trim().toLowerCase();
      let shown = 0;
      for (const o of opts) {
        // 中英文都能搜
        if (k && !((o.path + ' ' + o.desc + ' ' + o.descEn).toLowerCase().includes(k))) continue;
        if (shown++ > 300) break;
        const it = el('div', 'pitem');
        const top = el('div', 'pitemtop');
        top.appendChild(el('span', 'pp', o.path));
        top.appendChild(el('span', 'tag t-' + (o.type === 'enum' ? 'enum' : o.type), o.type));
        if (o.deprecated) top.appendChild(el('span', 'tag dep', '废弃'));
        if (o.required) top.appendChild(el('span', 'tag req', '必填'));
        if (o.def !== undefined && o.def !== null) top.appendChild(el('span', 'tag t-str', '有默认值'));
        if (o.enum && o.enum.length) top.appendChild(el('span', 'tag t-enum', `${o.enum.length} 个候选`));
        it.appendChild(top);
        if (o.desc) it.appendChild(el('div', 'pd', o.desc));
        it.onclick = () => {
          // 先选中，再由用户在详情区取值 —— 不再一点就插入
          for (const x of items.querySelectorAll('.pitem')) x.classList.remove('psel');
          it.classList.add('psel');
          showDetail(o);
        };
        items.appendChild(it);
      }
      if (!shown) items.appendChild(el('div', 'empty', '没有匹配的键'));
    };

    /// 详情区：完整说明 + 默认值占位 + 枚举可筛选列表
    const showDetail = (o) => {
      detail.innerHTML = '';
      detail.style.display = '';

      const t = el('div', 'pdetailhead');
      t.appendChild(el('span', 'pp', o.path));
      t.appendChild(el('span', 'tag t-' + (o.type === 'enum' ? 'enum' : o.type), o.type));
      detail.appendChild(t);

      // 说明（中文），可展开英文原文
      if (o.desc) {
        const d = el('div', 'pdesc');
        d.appendChild(setText(el('span', 'zh'), o.descZh || o.desc));
        if (o.descEn && o.descEn !== (o.descZh || o.desc)) {
          const tg = el('a', 'enorig', '原文');
          tg.href = '#';
          tg.onclick = (e) => {
            e.preventDefault();
            const old = d.querySelector('.enfull');
            if (old) { old.remove(); tg.textContent = '原文'; }
            else { d.appendChild(setText(el('div', 'enfull'), o.descEn)); tg.textContent = '收起'; }
          };
          d.appendChild(document.createTextNode(' '));
          d.appendChild(tg);
        }
        detail.appendChild(d);
      }
      if (o.doc) {
        detail.appendChild(externalLink(o.doc, o.doc, 'pdoc'));
      }

      // 取值区
      const vrow = el('div', 'panelrow');
      vrow.appendChild(el('label', null, '值'));
      let picked = null;

      if (o.enum && o.enum.length) {
        // 枚举：可筛选的候选列表
        const picker = enumPicker(o.enum, (v) => { picked = v; }, o.def);
        vrow.appendChild(picker);
        detail.appendChild(vrow);
        const hasDef = o.def !== undefined && o.def !== null;
        if (hasDef) {
          detail.appendChild(el('div', 'paneltip',
            `schema 默认值：${fmt(o.def)}（已在候选里标为「默认」）`));
        }
      } else {
        const inp = el('input');
        inp.type = 'text';
        inp.className = 'edval';
        // 默认值占位
        const hasDef = o.def !== undefined && o.def !== null;
        inp.placeholder = hasDef
          ? `默认值：${fmt(o.def)}`
          : `留空则用类型零值（${zeroValue(o.type)}）`;
        inp.oninput = () => { picked = inp.value; };
        vrow.appendChild(inp);
        detail.appendChild(vrow);
        if (hasDef) {
          const hint = el('div', 'paneltip', `schema 默认值：${fmt(o.def)}（留空即用默认值）`);
          detail.appendChild(hint);
        }
      }

      const acts = el('div', 'pacts');
      const go = el('button', 'primary', '添加这个键');
      go.onclick = () => {
        let value;
        if (o.enum && o.enum.length) {
          if (picked === null || picked === undefined) {
            alert('请先从候选列表里选一个值');
            return;
          }
          value = picked;
        } else {
          const raw = detail.querySelector('input.edval');
          const text = raw ? raw.value.trim() : '';
          value = text === '' ? zeroValue(o.type, o.def, o.enum) : coerce(text, o.type);
        }
        pushOp({ op: 'set', path: o.path.split('.'), value }, { rerender: true });
        EDITOR.panel = null;
        render();
      };
      const cancel = el('button', 'mini', '取消');
      cancel.onclick = () => {
        detail.style.display = 'none';
        detail.innerHTML = '';
        for (const x of items.querySelectorAll('.pitem')) x.classList.remove('psel');
      };
      acts.appendChild(go);
      acts.appendChild(cancel);
      detail.appendChild(acts);
    };

    search.oninput = () => draw(search.value);
    draw('');
    box.appendChild(list);
  } else {
    const row = el('div', 'panelrow');
    row.appendChild(el('label', null, '键路径'));
    const inp = el('input');
    inp.type = 'text';
    inp.placeholder = '例如 model 或 permissions.allow';
    row.appendChild(inp);
    box.appendChild(row);

    const row2 = el('div', 'panelrow');
    row2.appendChild(el('label', null, '类型'));
    const sel = el('select');
    for (const t of ['string', 'number', 'boolean', 'object', 'array']) {
      const o = el('option', null, t); o.value = t; sel.appendChild(o);
    }
    row2.appendChild(sel);
    box.appendChild(row2);

    const row3 = el('div', 'panelrow');
    row3.appendChild(el('label', null, '值'));
    const val = el('input');
    val.type = 'text';
    val.placeholder = '留空则用类型零值';
    row3.appendChild(val);
    box.appendChild(row3);

    const go = el('button', 'primary', '新增这个键');
    go.onclick = () => {
      const p = inp.value.trim().split('.').filter(Boolean);
      if (!p.length) { alert('请填写键路径'); return; }
      const text = val.value.trim();
      const value = text === '' ? zeroValue(sel.value) : coerce(text, sel.value);
      pushOp({ op: 'set', path: p, value }, { rerender: true });
      EDITOR.panel = null;
      render();
    };
    box.appendChild(go);
  }
  main.appendChild(box);
}

/// 当前文件属于哪个工具（用校验结果里的 tool 名反查）
function currentTool() {
  if (!DATA || !EDITOR.validation || !EDITOR.validation.tool) return null;
  return DATA.tools.find(t => t.display_name === EDITOR.validation.tool) || null;
}

// ── 新建配置文件 ──────────────────────────────────────

function renderNewFilePanel(main) {
  const box = el('div', 'panel');
  const head = el('div', 'panelhead');
  head.appendChild(el('span', null, '新建配置文件'));
  const close = el('button', 'mini', '关闭');
  close.onclick = () => { EDITOR.panel = null; render(); };
  head.appendChild(close);
  box.appendChild(head);

  const st = EDITOR.newFile || (EDITOR.newFile = { toolId: '', path: '', template: 'empty', candidates: [] });

  const row = el('div', 'panelrow');
  row.appendChild(el('label', null, '工具'));
  const sel = el('select');
  const opt0 = el('option', null, '（选择工具）'); opt0.value = ''; sel.appendChild(opt0);
  for (const t of (DATA ? DATA.tools : [])) {
    const o = el('option', null, t.display_name); o.value = t.id; sel.appendChild(o);
  }
  sel.value = st.toolId;
  sel.onchange = async () => {
    st.toolId = sel.value;
    st.path = '';
    st.candidates = st.toolId ? await invoke('new_file_candidates', { tool_id: st.toolId }) : [];
    render();
  };
  row.appendChild(sel);
  box.appendChild(row);

  if (st.toolId) {
    const row2 = el('div', 'panelrow');
    row2.appendChild(el('label', null, '路径'));
    const inp = el('input');
    inp.type = 'text';
    inp.placeholder = '配置文件的完整路径';
    inp.value = st.path;
    inp.oninput = () => { st.path = inp.value; };
    row2.appendChild(inp);
    box.appendChild(row2);

    if (st.candidates && st.candidates.length) {
      const cands = el('div', 'panelopts');
      cands.appendChild(el('div', 'paneltip', '常用位置：'));
      for (const c of st.candidates) {
        const b = el('button', 'mini cand', c);
        b.onclick = () => {
          st.path = c.startsWith('<项目根>/') ? c : c;
          render();
        };
        cands.appendChild(b);
      }
      box.appendChild(cands);
    }

    const row3 = el('div', 'panelrow');
    row3.appendChild(el('label', null, '初始内容'));
    const tsel = el('select');
    for (const [v, t] of [['empty', '空对象 {}'], ['list', '空数组 []']]) {
      const o = el('option', null, t); o.value = v; tsel.appendChild(o);
    }
    tsel.value = st.template;
    tsel.onchange = () => { st.template = tsel.value; };
    row3.appendChild(tsel);
    box.appendChild(row3);

    const go = el('button', 'primary', '创建并打开');
    go.onclick = async () => {
      if (!st.path.trim()) { alert('请填写路径'); return; }
      try {
        await invoke('create_file', { path: st.path.trim(), template: st.template });
        EDITOR.panel = null;
        await openFile(st.path.trim());
      } catch (e) { alert('创建失败：' + e); }
    };
    box.appendChild(go);
  }
  main.appendChild(box);
}

function renderEditor(main) {
  const f = EDITOR.file;
  if (!f) {
    main.appendChild(el('div', 'empty', '还没有打开文件。到「扫描磁盘」页挑一个，或在左侧输入路径。'));
    return;
  }

  const head = el('div', 'edhead');
  const title = el('div', 'edpath');
  title.appendChild(el('span', 'tag t-str', (f.format || '').toUpperCase()));
  title.appendChild(el('span', null, f.path));
  head.appendChild(title);

  const acts = el('div', 'edacts');
  const status = el('span', 'edstatus');
  if (EDITOR.dirty) status.textContent = `${EDITOR.ops.length} 处未保存修改`;
  acts.appendChild(status);

  const add = el('button', null, '+ 新增键');
  add.id = 'btn-add';
  add.onclick = () => { EDITOR.panel = 'add'; EDITOR.addCtx = { parent: [], kind: 'key' }; render(); };
  acts.appendChild(add);

  const undo = el('button', null, '撤销');
  undo.id = 'btn-undo';
  undo.disabled = !EDITOR.undo.length;
  undo.onclick = undoOp;
  acts.appendChild(undo);

  const redo = el('button', null, '重做');
  redo.id = 'btn-redo';
  redo.disabled = !EDITOR.redo.length;
  redo.onclick = redoOp;
  acts.appendChild(redo);

  const check = el('button', null, '校验');
  check.onclick = () => runValidation();
  acts.appendChild(check);

  const prev = el('button', null, '预览差异');
  prev.id = 'btn-preview';
  prev.disabled = !EDITOR.ops.length;
  prev.onclick = async () => {
    try {
      const res = await invoke('preview_edit', { path: f.path, ops: EDITOR.ops });
      EDITOR.newText = res.new_text;
      EDITOR.diff = res.diff || [];
      render();
    } catch (e) { alert('预览失败：' + e); }
  };
  acts.appendChild(prev);

  const save = el('button', 'primary', '保存到文件');
  save.id = 'btn-save';
  save.disabled = !EDITOR.ops.length;
  save.onclick = async () => {
    // 保存前强制校验：有解析错误直接拦下，有错误则要用户确认
    let v = null;
    try {
      v = await invoke('validate_edit', { path: f.path, ops: EDITOR.ops });
    } catch (e) {
      alert('校验失败：' + e);
      return;
    }
    EDITOR.validation = v;
    EDITOR.stale = false;
    renderValidationPanel();
    if (v.parse_error) {
      alert('保存已取消：修改后的内容无法解析\n' + v.parse_error);
      return;
    }
    if (v.error_count > 0) {
      const detail = (v.issues || [])
        .filter(i => i.level === 'error')
        .map(i => '• ' + i.path + ' — ' + i.message)
        .join('\n');
      if (!confirm(`校验发现 ${v.error_count} 个错误，仍然保存吗？\n\n${detail}`)) {
        return;
      }
    }
    try {
      const res = await invoke('commit_edit', {
        path: f.path, ops: EDITOR.ops, base_fingerprint: f.fingerprint,
      });
      EDITOR.file = await invoke('open_file', { path: f.path });
      EDITOR.ops = []; EDITOR.undo = []; EDITOR.redo = [];
      EDITOR.newText = null; EDITOR.diff = []; EDITOR.dirty = false;
      render();
      await runValidation();
      alert('已保存' + (res.backup ? `\n备份：${res.backup}` : ''));
    } catch (e) {
      // 后端冲突错误是结构化 JSON 字符串
      const msg = String((e && e.message) || e);
      let payload = null;
      try { payload = JSON.parse(msg); } catch (_) { payload = null; }
      if (payload && payload.kind === 'conflict') {
        const choice = await askChoice(
          '文件已被其他程序修改',
          `打开之后这个文件又变了（指纹 ${payload.expected} → ${payload.actual}）。\n` +
          '继续保存会覆盖对方的改动。',
          [
            { id: 'reload', label: '放弃我的修改，重新载入', desc: '读取磁盘上的最新内容，丢弃当前未保存的改动' },
            { id: 'overwrite', label: '仍然覆盖保存', desc: '用我的版本覆盖磁盘上的文件' },
            { id: 'cancel', label: '先不保存', desc: '什么都不做，改动继续留在编辑器里' },
          ]);
        if (choice === 'reload') {
          EDITOR.ops = []; EDITOR.undo = []; EDITOR.redo = [];
          EDITOR.newText = null; EDITOR.diff = []; EDITOR.dirty = false;
          EDITOR.file = await invoke('open_file', { path: f.path });
          render();
        } else if (choice === 'overwrite') {
          try {
            const res2 = await invoke('commit_edit', { path: f.path, ops: EDITOR.ops });
            EDITOR.file = await invoke('open_file', { path: f.path });
            EDITOR.ops = []; EDITOR.undo = []; EDITOR.redo = [];
            EDITOR.newText = null; EDITOR.diff = []; EDITOR.dirty = false;
            render();
            alert('已覆盖保存' + (res2.backup ? `\n备份：${res2.backup}` : ''));
          } catch (e2) { alert('保存失败：' + e2); }
        }
        return;
      }
      alert('保存失败：' + msg);
    }
  };
  acts.appendChild(save);

  const revert = el('button', null, '放弃修改');
  revert.id = 'btn-revert';
  revert.disabled = !EDITOR.ops.length;
  revert.onclick = () => {
    EDITOR.ops = []; EDITOR.newText = null; EDITOR.diff = []; EDITOR.dirty = false;
    render();
    runValidation();
  };
  acts.appendChild(revert);
  head.appendChild(acts);
  main.appendChild(head);

  const valbox = el('div', 'valbox');
  valbox.id = 'valbox';
  main.appendChild(valbox);
  renderValidationPanel();

  if (EDITOR.diff && EDITOR.diff.length) {
    const d = el('div', 'diffbox');
    d.appendChild(el('div', 'diffhead', '保存前差异预览（确认无误后点「保存到文件」）'));
    for (const line of EDITOR.diff) {
      const l = el('div', 'diffline');
      l.textContent = line;
      d.appendChild(l);
    }
    main.appendChild(d);
  }

  const tree = el('div', 'edtree');
  renderEdNode(tree, editorView(), [], f);
  main.appendChild(tree);

  if (EDITOR.panel === 'add') {
    renderAddPanel(main);
    // 面板在长树下面，滚到可见处，省得用户自己找
    const p = main.querySelector('.panel');
    if (p) p.scrollIntoView({ block: 'nearest' });
  }
  if (EDITOR.panel === 'newfile') renderNewFilePanel(main);
}

// ── 编辑器：本地视图（把待保存的操作应用到树上，改动立刻可见） ──

const deepClone = (v) => (v === undefined ? v : JSON.parse(JSON.stringify(v)));

/// 沿路径找到父节点
function parentOf(root, path) {
  let c = root;
  for (let i = 0; i < path.length - 1; i++) {
    if (c == null || typeof c !== 'object') return null;
    c = c[Array.isArray(c) ? Number(path[i]) : path[i]];
  }
  return c;
}

/// 沿路径找到父节点，**顺手把缺失的中间层补成对象**。
///
/// 新增一个父级还不存在的嵌套键时（比如往没有 `[sandbox]` 表的配置里加
/// `sandbox.auto_allow_bash`），原来的实现会走到 undefined 就返回 null，
/// 于是这次修改被**静默丢掉** —— 用户点了「添加」什么都没发生。
/// TOML 落盘那边本来就会自动建表，这里跟它对齐。
function parentOfCreate(root, path) {
  let c = root;
  for (let i = 0; i < path.length - 1; i++) {
    if (c == null || typeof c !== 'object') return null;
    const key = Array.isArray(c) ? Number(path[i]) : path[i];
    let nxt = c[key];
    if (nxt == null || typeof nxt !== 'object') {
      // 已有值是标量 → 不能在它下面建表，交给落盘时报错
      if (nxt !== undefined && nxt !== null) return null;
      nxt = {};
      c[key] = nxt;
    }
    c = nxt;
  }
  return c;
}

/// 沿路径找到节点本身
function nodeAt(root, path) {
  let c = root;
  for (const seg of path) {
    if (c == null || typeof c !== 'object') return null;
    c = c[Array.isArray(c) ? Number(seg) : seg];
  }
  return c;
}

function setAt(root, path, value) {
  if (!path.length) return value;
  // 父级不存在时自动补中间层（跟 TOML 落盘的行为一致）
  const p = parentOfCreate(root, path);
  if (p == null) return root;
  const last = path[path.length - 1];
  if (Array.isArray(p)) p[Number(last)] = value;
  else p[last] = value;
  return root;
}

function removeAt(root, path) {
  if (!path.length) return root;
  const p = parentOf(root, path);
  if (p == null) return root;
  const last = path[path.length - 1];
  if (Array.isArray(p)) p.splice(Number(last), 1);
  else delete p[last];
  return root;
}

function insertAt(root, path, index, value) {
  const arr = nodeAt(root, path);
  if (!Array.isArray(arr)) return root;
  arr.splice(Math.min(index, arr.length), 0, deepClone(value));
  return root;
}

function moveAt(root, path, from, to) {
  const arr = nodeAt(root, path);
  if (!Array.isArray(arr) || from < 0 || from >= arr.length) return root;
  const t = Math.max(0, Math.min(to, arr.length - 1));
  const [it] = arr.splice(from, 1);
  arr.splice(t, 0, it);
  return root;
}

/// 当前编辑器该显示的树 = 原文件 + 待保存操作
function editorView() {
  if (!EDITOR.file) return null;
  let cur = deepClone(EDITOR.file.value);
  for (const op of EDITOR.ops) {
    if (op.op === 'set') cur = setAt(cur, op.path, deepClone(op.value));
    else if (op.op === 'remove') cur = removeAt(cur, op.path);
    else if (op.op === 'insert') cur = insertAt(cur, op.path, op.index, op.value);
    else if (op.op === 'move') cur = moveAt(cur, op.path, op.from, op.to);
  }
  return cur;
}

// ── 编辑器：结构树 ────────────────────────────────────

/// 已展开的折叠块路径（跨重绘保持，避免改一次就跳回顶部）
let EXPANDED = new Set();

/// 一个值单元格：标量 → 输入框；对象/数组 → 可展开的树
function renderEdValue(value, path, file) {
  if (value !== null && typeof value === 'object') {
    const d = el('details', 'ednode');
    const key = pathKey(path);
    // 前两层默认展开，再深就收起；用户手动展开过的保持展开
    d.open = EXPANDED.has(key) || path.length <= 2;
    d.ontoggle = () => {
      if (d.open) EXPANDED.add(key); else EXPANDED.delete(key);
    };
    const s = el('summary');
    s.appendChild(el('span', 'tag ' + (Array.isArray(value) ? 't-arr' : 't-obj'),
      Array.isArray(value) ? `array · ${value.length} 项` : `object · ${Object.keys(value).length} 键`));
    // 对象 / 数组本身在 schema 里也可能有说明
    const oinfo = schemaInfoFor(toolForFile(file && file.path), path);
    const odesc = descOf(oinfo);
    if (odesc) {
      const od = el('span', 'eddesc', odesc);
      od.title = odesc;
      s.appendChild(od);
    }
    d.appendChild(s);
    const c = el('div', 'edkids');
    renderEdNode(c, value, path, file);
    d.appendChild(c);
    return d;
  }
  const inp = el('input');
  inp.type = 'text';
  inp.className = 'edval';
  inp.value = valueToInput(value);
  inp.oninput = () => {
    pushOp({ op: 'set', path, value: inputToValue(inp.value, value) });
  };
  return inp;
}

/// 密钥字段：默认显示掩码，点「显示明文」才取真值
function renderEdSecret(value, path, file) {
  const k = pathKey(path);
  if (EDITOR.revealed[k] !== undefined) {
    const inp = el('input');
    inp.type = 'text';
    inp.className = 'edval shown';
    inp.value = EDITOR.revealed[k];
    inp.oninput = () => {
      EDITOR.revealed[k] = inp.value;
      pushOp({ op: 'set', path, value: inp.value });
    };
    return inp;
  }
  const wrap = el('div', 'edsecret');
  wrap.appendChild(el('span', 'edval mask', typeof value === 'string' ? value : '••••••••'));
  const btn = el('button', 'mini', '显示明文');
  btn.onclick = async () => {
    try {
      const r = await invoke('reveal_secret', { path: file.path, key_path: k });
      EDITOR.revealed[k] = r.value;
      render();
    } catch (e) { alert('读取失败：' + e); }
  };
  wrap.appendChild(btn);
  return wrap;
}

/// 值单元格入口：密钥走脱敏分支
function renderEdCell(value, path, file) {
  if (isSecretPath(path)) return renderEdSecret(value, path, file);
  return renderEdValue(value, path, file);
}

function renderEdNode(parent, node, path, file) {
  if (node && typeof node === 'object' && !Array.isArray(node)) {
    for (const [k, v] of Object.entries(node)) {
      const p = path.concat(k);
      parent.appendChild(editableRow(k, v, p, file));
    }
  } else if (Array.isArray(node)) {
    // 数组容器：头部长「+ 添加元素」，每项有上移/下移/删除
    const box = el('div', 'edarr');
    const head = el('div', 'edarrhead');
    // 根级数组没有外层 details 包裹，标题得自己写；嵌套的由外层 summary 显示
    if (path.length === 0) head.appendChild(el('span', 'tag t-arr', `array · ${node.length} 项`));
    const add = el('button', 'mini', '+ 添加元素');
    add.onclick = () => { EDITOR.panel = 'add'; EDITOR.addCtx = { parent: path, kind: 'array' }; render(); };
    head.appendChild(add);
    box.appendChild(head);

    const list = el('div', 'edkids');
    node.forEach((v, i) => {
      const p = path.concat(String(i));
      const row = el('div', 'edrow editem');
      const name = el('div', 'edkey');
      name.appendChild(el('span', 'seg', '[' + i + ']'));
      // 数组元素的说明在 schema 里是 `xxx[]`
      const einfo = schemaInfoFor(toolForFile(file && file.path), p);
      const edesc = descOf(einfo);
      if (edesc) {
        const ed = setText(el('div', 'eddesc'), edesc);
        ed.title = edesc;
        name.appendChild(ed);
      }
      row.appendChild(name);
      row.appendChild(renderEdCell(v, p, file));

      const ctl = el('div', 'edctl');
      const up = el('button', 'mini', '↑');
      up.title = '上移';
      up.disabled = i === 0;
      up.onclick = () => pushOp({ op: 'move', path, from: i, to: i - 1 }, { rerender: true });
      const dn = el('button', 'mini', '↓');
      dn.title = '下移';
      dn.disabled = i === node.length - 1;
      dn.onclick = () => pushOp({ op: 'move', path, from: i, to: i + 1 }, { rerender: true });
      const del = el('button', 'eddel', '删除');
      del.onclick = () => pushOp({ op: 'remove', path: p }, { rerender: true });
      ctl.appendChild(up); ctl.appendChild(dn); ctl.appendChild(del);
      row.appendChild(ctl);
      list.appendChild(row);
    });
    box.appendChild(list);
    parent.appendChild(box);
  }
}

function editableRow(key, value, path, file) {
  const row = el('div', 'edrow');
  const name = el('div', 'edkey');
  name.appendChild(el('span', 'seg', key));
  // schema 里的说明：光有键名看不出这个配置项是干嘛的
  const info = schemaInfoFor(toolForFile(file && file.path), path);
  const desc = descOf(info);
  if (desc) {
    const d = setText(el('div', 'eddesc'), desc);
    d.title = desc;
    name.appendChild(d);
  }
  row.appendChild(name);
  row.appendChild(renderEdCell(value, path, file));

  const ctl = el('div', 'edctl');
  const del = el('button', 'eddel', '删除');
  del.onclick = () => pushOp({ op: 'remove', path }, { rerender: true });
  ctl.appendChild(del);
  row.appendChild(ctl);
  return row;
}

// ── 公共 ──────────────────────────────────────────────

function openExternal(url) {
  if (isDesktopApp()) invoke('open_external', { url });
}

function render() {
  const side = document.getElementById('side');
  const main = document.getElementById('main');
  side.innerHTML = '';
  main.innerHTML = '';
  renderNav();

  if (PAGE === 'scan') {
    renderScanSide();
    renderScan(main);
    return;
  }
  if (PAGE === 'edit') {
    renderEditSide();
    renderEditor(main);
    return;
  }
  if (!DATA) { main.appendChild(el('div', 'empty', '正在加载…')); return; }
  renderDictSide();
  renderDict(main);
}

function boot() {
  document.getElementById('lang').onclick = () => {
    lang = lang === 'zh-CN' ? 'en' : 'zh-CN';
    document.getElementById('lang').textContent = lang === 'zh-CN' ? 'EN' : '中文';
    render();
  };
  applyDeepLink();
  render();
}

function installRefresh() {
  const header = document.querySelector('header');
  const btn = document.createElement('button');
  btn.id = 'refresh';
  btn.textContent = '刷新 schema';
  btn.onclick = async () => {
    btn.disabled = true;
    btn.textContent = '拉取中…';
    try {
      DATA = await invoke('refresh_schemas');
      render();
      btn.textContent = '已刷新';
      setTimeout(() => { btn.textContent = '刷新 schema'; }, 1500);
    } catch (e) {
      btn.textContent = '刷新失败';
      setTimeout(() => { btn.textContent = '刷新 schema'; }, 2500);
    } finally {
      btn.disabled = false;
    }
  };
  header.insertBefore(btn, document.getElementById('lang'));
}

function applyDeepLink() {
  const p = new URLSearchParams(location.search);
  if (p.get('tool')) {
    const i = DATA ? DATA.tools.findIndex(t => t.id === p.get('tool')) : -1;
    if (i >= 0) cur = i;
  }
  if (p.get('filter')) filt = p.get('filter');
  if (p.get('q')) q = p.get('q');
  if (p.get('lang') === 'en') {
    lang = 'en';
    document.getElementById('lang').textContent = '中文';
  }
}

if (isDesktop()) {
  const main = document.getElementById('main');
  main.innerHTML = '<div class="empty">正在加载配置字典…</div>';
  installRefresh();
  invoke('get_data').then(data => {
    DATA = data;
    boot();
  }).catch(err => {
    main.innerHTML = `<div class="empty">加载失败：${err}</div>`;
  });
} else {
  DATA = JSON.parse(document.getElementById('payload').textContent);
  boot();
}
