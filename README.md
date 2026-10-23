# AI CLI 配置字典

[![CI](https://github.com/gztcode/ai-cli-config-dictionary/actions/workflows/ci.yml/badge.svg)](https://github.com/gztcode/ai-cli-config-dictionary/actions/workflows/ci.yml)
[![Release](https://github.com/gztcode/ai-cli-config-dictionary/actions/workflows/release.yml/badge.svg)](https://github.com/gztcode/ai-cli-config-dictionary/actions/workflows/release.yml)

跨平台桌面应用（Rust + Tauri 2）：浏览 Claude Code、Codex、Gemini CLI、Grok Build、OpenCode、OpenClaw、Hermes、Pi、MiniMax Code 的**全部配置项**，查看每个 key 的官方说明与文档链接；**扫描整机所有项目**找出这些工具的配置文件；**独立的编辑子页面**，点开就能改——能新增、能增删数组、能撤销、能校验、能预览差异、能安全写回。

> 独立原生应用（有 macOS/Windows/Linux 各自的安装包与原生窗口），不是网页。

---

## 快速开始

```bash
./启动预览.command              # 双击运行（或在终端执行）
./启动预览.command --refresh    # 先拉取最新 schema 再启动
./打包.app.sh                    # 重新构建 .app
```

产物：
- `desktop/src-tauri/target/release/bundle/macos/AI配置字典.app` — macOS 独立应用
- Windows：`--bundles msi`；Linux：`--bundles deb` / `appimage`

命令行版：

```bash
cargo run -- discover   # 本机各工具配置现状
cargo run -- fetch      # 拉取 schema 到 cache/
cargo run -- build      # schema → IR（out/ir/*.json）
cargo run -- check      # 用真实配置校验
cargo run -- preview    # 导出单文件 HTML（离线可看）

cargo run -- scan                          # 扫描用户目录，找出所有 AI CLI 配置文件与项目
cargo run -- scan --root ~/Projects --content-scan
cargo run -- edit ~/.claude/settings.json --set 'model=claude-opus-4' --dry-run
cargo run -- serve                         # HTTP 桥：真实前端 + 真实命令（调试/脚本化）
```

---

## 三个页面

顶栏切换，三个是**平级的独立页面**（不是同一页里换内容）。

### 1. 配置字典（默认页，全量）

按工具列出 schema 里的全部 key，每个 key 带**类型、官方说明、文档链接、枚举、默认值、是否废弃**，并标出本机配置里的**当前值**与**校验问题**。

**说明文字已全量中文化**（1926 条带说明的键 100% 覆盖）。按 fallback 链「请求语言 → 覆盖层 → 英文原文」**择一显示**——中文模式下只显示译文，原文收进一个「原文」链接里，点开就在下方对照；顶栏 [EN] 可整体切回英文原文。

**字典 ↔ 本机是打通的**：

- 每条 key 标 **「已配置」**，点「本机已配置」筛选只看自己用到的
- 已配置的 key 直接显示**当前值**，点一下就跳进编辑器打开对应文件
- 无公开 schema 的工具，字典区会说明原因、给出官方文档入口，并直接列出本机已知配置位置（不编造键位、不伪造文档锚点）

本机配置文件路径后面直接跟一个「编辑」链接 —— 点它就跳到编辑子页面。

### 2. 扫描磁盘

**一次遍历同时产出两样东西**：发现的项目，和项目里的 AI CLI 配置文件。

- **项目发现**：目录里有 `.git` / `package.json` / `Cargo.toml` / `go.mod` / `pyproject.toml` / `Makefile` 等标志文件就算一个项目
- **文件命中**：已知文件名（`settings.json` / `config.toml` / `.mcp.json` / `opencode.json` …）+ 内容特征（`ANTHROPIC_BASE_URL`、`mcpServers`、`model_providers`）
- **归属判定**：通用文件名（`settings.json` / `config.yaml`）只有在 AI CLI 的配置目录里才算命中 —— 否则 `miniprogram/pages/settings/settings.json` 这类业务文件会被误当成配置
- **范围**：用户目录 / 常见项目目录 / 全盘 / 自定义路径
- **结果可按工具筛选**

结果**按项目分组**显示，每个项目标出标志文件和配置数量；不属于任何项目的（用户级全局配置）单独归一组。可一键只看「带配置的项目」。

> 本机实测：全用户目录 **10 万文件 / 3.5 秒**，发现 89 个项目、其中 18 个带 AI CLI 配置。

### 3. 编辑器（独立子页面）

- 侧栏：当前文件、**按路径打开任意文件**、最近打开列表、**新建配置文件**
- **每个 key 都带 schema 说明**（中文，来自官方 schema）：树里 `model` 下面直接写「覆盖 Claude Code 使用的默认模型…」，不用去查文档
- **新增键**：从 schema 里挑（可搜索的列表，每项都带**中文说明**、类型、是否有默认值、枚举候选数），也可以自由输入路径 + 类型
  - 选中之后**不是立刻插入**，而是展开详情区：完整说明（含「原文」对照）、官方文档链接、取值区
  - 取值区：有默认值就**用默认值占位**（`默认值：30`），留空即用默认值
  - 有枚举的键，值是**可筛选的候选列表**（每项带枚举值说明，默认值标「默认」）
- **数组可增删改排**：添加元素、删除、上移、下移；添加元素时也显示这个数组是干嘛的
- **嵌套结构逐字段编辑**：对象和数组都能展开一层层改，不用重写整块 JSON
- **撤销 / 重做**（`Ctrl+Z` / `Ctrl+Shift+Z`）
- **密钥默认脱敏**：API key / token 这类字段只显示掩码，点「显示明文」才取真值
- **外部改动冲突检测**：文件在你编辑期间被别人改了，保存会被拦下，给「放弃我的修改 / 仍然覆盖 / 先不保存」三选一
- **保存前按 schema 校验**，有错误会拦下来并列出具体问题
- **保存前给出逐行差异预览**，确认无误再落盘
- 落盘用**同目录临时文件 + rename** 原子替换，原文件在同级 `.cfg-backups/` 留一份备份

**写回保留原格式**：缩进、注释、键顺序、YAML 结构一律原样保留，只改你动过的那几个字节。

**密钥安全阀**：脱敏后的掩码值不会被当成真值写回文件 —— 用户没碰过的密钥字段，保存时原样保留。

---

## 架构

```
registry.toml            各家工具注册信息（数据源 / 配置路径 / 格式）
schemas/                 5 家「无官方 schema」工具的派生 schema（随仓库分发）
src/
  ir.rs                  IR 定义 —— GUI 的唯一数据契约
  schema.rs              JSON Schema → IR 转换器（核心）
  config.rs              JSON / TOML / YAML 统一读成 serde_json::Value
  validate.rs            基于 IR 的配置校验
  secret.rs              密钥识别与脱敏（分级词边界匹配）
  app.rs                 数据装配层（CLI / HTML / 桌面 App 共用）
  commands.rs            前端命令实现（桌面 App IPC 与 HTTP 桥共用同一份）
  scan.rs                磁盘扫描（自写多线程遍历器）
  jsonc.rs               自写 JSON/JSONC 解析器（记录每个值的字节区间）
  edit.rs                保格式编辑（JSON 字节级替换 / TOML toml_edit / YAML 行级定点）
  serve.rs               HTTP 桥（把同一套命令暴露成 HTTP）
  preview.rs             单文件 HTML 导出
  ui/app.css, app.js     渲染层（HTML 导出 / 桌面 App / HTTP 桥共用同一份）
  i18n/zh-CN.json        多语言覆盖层
desktop/src-tauri/       Tauri 2 桌面应用（IPC 适配层）
tools/translate.py       全量中文词条生成（批量 LLM 翻译 + 术语表 + 质量闸门）
tools/derive_schema.py   从各家官方来源派生 schema（表格 / Python 定义 / Zod / npm 包）
tools/make_icon.py       图标生成（纯标准库）
tests/
  edit_roundtrip.rs      保格式编辑回归测试（29 个）
  secret_test.rs         密钥识别与脱敏测试（8 个）
  contract.rs            前端 ↔ Rust 的 JSON 契约测试（9 个）
  scan_test.rs           扫描器回归测试（7 个）
  i18n_test.rs           内置中文词条质量测试（6 个）
```

**关键设计一：GUI 只消费 IR，不直接碰 JSON Schema。**
校验器也跑在 IR 上而非原始 schema 上——这本身就是验证：IR 携带的信息足够完成类型/枚举/范围/正则/必填/未知键判定，界面只拿 IR 就能实现保存前拦截。

**关键设计二：命令实现不依赖 Tauri。**
`src/commands.rs` 里的函数被桌面 App 的 IPC 和 CLI 的 HTTP 桥共用。前端只认 `invoke(cmd, args)` 这一层协议，换传输方式前端一行都不用改。

**关键设计三：UI 只有一份源码。**
`src/ui/` 下的 CSS/JS 被静态导出、桌面 App（build.rs 复制）、HTTP 桥三处共用，不会出现「网页版和桌面版长得不一样」。

**关键设计四：脱敏发生在服务端。**
`open_file` 返回给前端的配置树里密钥已经是掩码，**且不返回原始文本**——明文密钥不会进 WebView，也不会进静态 HTML 导出。只有用户点「显示明文」才通过 `reveal_secret` 单次取回。

---

## IR 契约

每个节点带：

| 字段 | 用途 |
|---|---|
| `path` / `name` | 键定位 |
| `value_type` | string/number/integer/boolean/object/array/enum/null/any |
| `description` | 官方说明（英文原文） |
| `doc_urls[]` | 官方文档链接（带锚点） |
| `enum_values[]` | 枚举取值 + 各自说明 |
| `default` / `examples[]` | 默认值与示例 |
| `constraints` | min/max/pattern/format/length/items |
| `required` / `deprecated` | 必填与废弃标记 |
| `free_form` | 该对象允许任意子键（= 手输自定义 key 的合法区域） |
| `translations{}` | locale → 描述（多语言覆盖层） |
| `children[]` | 递归子键 |

---

## schema 来源：两类

九家工具的 schema 来源分两类，都在 `registry.toml` 里声明：

**① 官方发布的机器可读 schema**（联网抓取，带缓存与离线种子）

| 工具 | 来源 | 说明 |
|---|---|---|
| Claude Code | SchemaStore | `claude-code-settings.json` |
| Codex CLI | 官方仓库 | `openai/codex` 仓库里的 `codex-rs/core/config.schema.json`（官网那份被 Cloudflare 挡 403，仓库里同一份可直连） |
| Gemini CLI | 官方仓库 | `google-gemini/gemini-cli` 的 `schemas/settings.schema.json` |
| OpenCode | 官方 | `opencode.ai/config.json` |

**② 从官方来源派生的 schema**（随仓库分发，`schemas/`）

这五家没有发布机器可读的 schema —— 它们的 schema 是构建时从源码生成的，不落文件。
但**官方文档里有结构化的字段表**，所以由 `tools/derive_schema.py` 从官方来源派生，
**只取真实的键位、类型、默认值、说明，不编造任何键**：

| 工具 | 派生来源 | 得到的键 |
|---|---|---|
| Grok Build | 官方仓库 `26-config-reference.md` 的字段表 | 384 叶子，**说明覆盖 100%** |
| Pi | 官方 npm 包里的 `docs/settings.md` 字段表 | 71 叶子，**说明覆盖 100%** |
| Hermes Agent | 官方仓库 `config_defaults.py` 的 `DEFAULT_CONFIG` + 源码注释（注释即官方文档） | 925 叶子，说明覆盖 49% |
| OpenClaw | 官方 npm 包里的 Zod schema（结构/枚举/默认值）＋ 官方生成式文档里的字段说明 | 1553 叶子，说明覆盖 1% |
| MiniMax Code | 官方 npm 包 `@minimax-ai/code` 的配置默认值 | 46 叶子 |

> **为什么 OpenClaw / MiniMax Code 的说明很少**：这两家的字段文档只存在于**散文**里
> （OpenClaw 的文档是叙述式页面，MiniMax Code 的包里连字段表都没有）。
> 我们不拿猜测去凑 —— 结构、类型、枚举、默认值照实给，说明只给能从官方文档确证的部分。

---

## 自动打包（GitHub Actions）

仓库里有两条工作流：

| 工作流 | 触发 | 做什么 |
|---|---|---|
| `.github/workflows/ci.yml` | 每次 push / PR | 在 ubuntu / macOS / Windows 上跑 `cargo test --release`，另加 `cargo fmt --check` 和 clippy |
| `.github/workflows/release.yml` | 推送 `v*` tag | **先在三个平台把测试跑一遍 → 全绿才打包** → 产出各平台安装包 → 自动创建 GitHub Release |

发版的硬约束是 job 之间的依赖：`test → build → release`，**测试不过就不会产出任何安装包**。

打 tag 即可发版：

```bash
git tag v0.2.0 && git push origin v0.2.0
```

产出的安装包：

| 平台 | 文件 |
|---|---|
| macOS (Apple Silicon) | `AI配置字典-aarch64-apple-darwin.zip` + `.dmg` |
| macOS (Intel) | `AI配置字典-x86_64-apple-darwin.zip` + `.dmg` |
| Windows (x64) | `.msi` |
| Linux (x64) | `.deb` + `.AppImage` |

另外还附上纯 Rust 的命令行版 `cfg-explorer-<target>`（无 GUI 依赖，可在服务器上跑 `scan` / `check` / `edit`）。

> - macOS 包**未做代码签名**，首次打开需要「右键 → 打开」。
> - Windows 包未签名，SmartScreen 可能提示。
> - CI 上会删掉 `.cargo/config.toml`（那是给国内网络用的 crates.io 镜像），GitHub runner 直连 crates.io 更快。

---

## 实测数据（本机，2026-10-22）

| 工具 | schema 来源 | 总键 | 叶子键 | 有说明 | 有枚举 | 有默认值 |
|---|---|---|---|---|---|---|
| Claude Code | SchemaStore | 1494 | 1183 | 1314 | 263 | 81 |
| Codex CLI | 官方仓库 | 307 | 296 | 94 | 0 | 9 |
| Gemini CLI | 官方仓库 | 575 | 399 | 457 | 25 | 235 |
| OpenCode | 官方 | 719 | 576 | 155 | 257 | 0 |
| Grok Build | 文档派生 | 453 | 384 | 391 | 1 | 0 |
| OpenClaw | 官方包（Zod） | 2266 | 1553 | 30 | 244 | 4 |
| Hermes Agent | 文档派生 | 1097 | 925 | 531 | 0 | 0 |
| Pi | 文档派生 | 80 | 71 | 71 | 0 | 0 |
| MiniMax Code | 官方包 | 64 | 46 | 0 | 0 | 0 |

合计 **5571 个叶子键**，其中 **3043 个键带说明**（含数组节点与对象节点），全部有中文译文。

**结论：文档链接无法统一收割。** 只有 Claude Code 的 schema 内嵌了带锚点的官方文档 URL（423 个键分布在 53 个页面上）；其余工具一个带锚点的链接都没有。产品上必须给「无精确链接」的键准备降级策略（指向该工具的文档索引页），而不是伪造精确锚点。

---

## 多语言

`src/i18n/zh-CN.json` 是覆盖层，键形如 `claude-code:model`。

- **全量中文**：`tools/translate.py` 把 schema 里 **3043** 条有说明的键（叶子 + 数组节点 + 对象节点，字典页会一并渲染）去重成 **2195** 条唯一英文文本批量翻译，术语表保证全库一致，**质量闸门**校验反引号内容、环境变量名、占位符必须原样保留
- **增量**：源文本变了才重译（`tools/.translate-cache.json` 存 hash）
- **升级不丢词条**：应用启动时把内置的新词条**合并**进用户目录里的覆盖层，用户自己改过的条目不会被覆盖
- fallback 链：请求语言 → 覆盖层 → 英文原文 → 「暂无官方说明」

```bash
python3 tools/translate.py            # 增量翻译缺失的
python3 tools/translate.py --check    # 只校验现有覆盖层的占位符完整性
python3 tools/translate.py --force    # 忽略缓存全部重译
```

---

## 派生 schema 的踩坑

1. **文档表格的列名不统一。** Pi 是 `Setting|Type|Default|Description`，Grok Build 是 `Key|Type/Values|Requirements|Managed|Details` —— 要按列名模糊匹配，不能写死列序。
2. **单元格里可能有转义的竖线。** Pi 的类型列写成 `"off" \| "streaming" \| "idle"`，直接按 `|` 切列会把整行切坏 —— 要先处理 `\|`。
3. **Python 的默认值靠 AST 求值，不能 exec。** `config_defaults.py` 会 `import hermes_cli.config_defaults_env`，直接 exec 会连带 import 一堆东西；而且 `DEFAULT_CONFIG` 里的值是 `_aux(...)` 这类函数调用，`ast.literal_eval` 也不行 → 只取模块顶层的**字面量与函数定义**，在受限命名空间里求值。
4. **注释有两种位置。** Hermes 的注释既可能写在键的**上方**（也可能是在讲下一级的子键：`# min_switch_reset_seconds: opt-in (0 = off)`），也可能写在**行尾**（`"max_size": 128,  # LRU entry cap`）—— 两种都要认。
5. **minified JS 的对象字面量有坑。** MiniMax Code 的包里是压缩过的 JS：布尔是 `!0`/`!1`、数字可能是 `1800*1e3` 这种表达式、还有 `...sr` 展开语法 → 写了个小解析器逐值扫描，遇到认不出来的表达式**只跳过那一个键**，不能整个对象作废。
6. **Zod 的 `toJSONSchema` 会丢掉 `.describe()` 之外的信息。** OpenClaw 的字段文档不在 Zod 里（只有 7 处 `.describe()`），而是在编译产物的 JSDoc 注释里 —— 注释无法可靠映射回路径，所以不硬凑，只用官方文档里能确证的那部分说明。

---

---

## GUI 技术选型（实测对比）

同机同版本实测两个最小骨架：

| 指标 | egui (eframe) | **Tauri 2** |
|---|---|---|
| Rust 二进制 | 13.5 MB | **3.8 MB** |
| macOS `.app` 包 | 13.5 MB | **3.9 MB** |
| 启动→首帧 | **0.15s** | 0.23s |

包体积 Tauri 小 3.4 倍；egui 启动快 0.08 秒——但 1494 个 key 对两者都毫无压力，这个差异无感知。**结论取 Tauri 2**：体积更小、UI 能复用同一份前端、且是这个领域的行业标准（CC Switch 等同类工具均如此）。

> macOS 上 Tauri 用系统 WebKit、Windows 用系统 WebView2，**不打包浏览器内核**，所以各平台体积一致。

---

## 转换器踩过的坑

1. **`anyOf` / `oneOf` 里的结构会被丢掉。** Claude Code 的 `hookCommand` 是 `anyOf: [{type:object, properties:{...}}, {...}]`，不归约分支就会整个丢掉 properties。→ 实现 `reduce_branches()`。
2. **`anyOf` 的 `required` 不能求并集。** 其语义是「满足任一分支」，求并集会误报成「command / prompt / url / server / tool 全都必填」。→ 只有 `allOf` 合并 required。
3. **`const` 是一种枚举。** `{"type":"string","const":"command"}` 要转成单值枚举。
4. **`$schema` / `$id` 是 JSON Schema 元键**，不是配置项，要过滤掉。

## 保格式编辑踩过的坑

1. **`Table::insert` 会抹掉 key 的装饰。** TOML 里 key 上的前导注释（`# ...`）存在 `Key` 的 decor 里，`Table::insert` 重建 Key 就把它丢了。→ 改成**原地替换 Item**，并把旧值上的行尾注释搬到新值上。
2. **递归时传错了 path 切片。** `toml_set(t, &parents[1..])` 把最后一段丢了，导致 `model_providers.default.base_url` 写成了 `model_providers.default = ...`。→ 应传 `&path[1..]`。
3. **YAML 序列要单独处理。** `tools.1.enabled` 这类路径里含下标，纯按「key: value」行匹配根本定位不到。→ 解析时给序列项单独建条目并维护下标计数器，删除时按整块删。
4. **JSON 删数组元素要连带处理逗号。** 删最后一个元素会留下 `[,]` 这种非法 JSON。→ 删完检查尾逗号并一并去掉。
5. **「尾逗号清理」不能对所有删除都执行。** 曾经删中间元素时也去扫尾逗号，把**后一个元素一起删掉**了，还留下一个孤零零的逗号。→ 只在删的是**最后一个**元素时才处理尾逗号。
6. **位置型操作不能按 path 去重。** 同一数组上的多次「插入 / 移动」如果按 path 合并，前一次会被吃掉，保存结果和界面显示对不上。→ 去重只作用于 `set` / `remove`。
7. **父级不存在时，新增嵌套键会被静默丢掉。** 前端的预览树 `parentOf()` 走到缺失的中间层就返回 null，于是「添加 `sandbox.auto_allow_bash`」点了跟没点一样；而 TOML 落盘那边会自动建表 —— 同一件事两套行为。→ 前端预览补齐中间层，JSON / YAML 落盘也跟着补（JSON 造嵌套对象字面量，YAML 逐层缩进补块）。
8. **缓存文件名不能只取 URL 末段。** 两家工具都用 `config.json` 结尾时会互相覆盖缓存。→ 缓存名加工具 id 前缀（`codex-config.schema.json`）。
9. **注册表是应用自带的数据，不能只在文件不存在时播种。** 以前那么写，升级后的桌面 App 还在用旧注册表 —— 没有 `schema_file` 字段，新接的五家工具全空，Codex 的地址也还是被 Cloudflare 挡的。→ 每次启动都用内置副本刷新，旧文件备份成 `registry.user.toml`。

---

## 翻译链路踩过的坑

1. **只译叶子键会留下 389 个英文节点。** 字典页会给对象节点（`permissions.allow`、`attribution` …）也渲染说明，只收 `kind == "leaf"` 就等于把这些漏掉。→ 收全部带说明的键，覆盖从 1537 条涨到 3043 条。
2. **术语表要和闸门配套。** 术语表里没有的词，模型会自由发挥，同一个词在不同条目里译法不一致。→ 术语表（~90 条）直接拼进提示词。
3. **闸门把大写强调词当成了标识符。** 规则是「所有全大写 token 一律保留」，本意是保护 `ANTHROPIC_API_KEY`，但 `DISCOURAGED` / `IGNORED` / `PAID` / `SKIPPED` 也匹配 —— 模型一翻译就判不合格，这些说明永远翻不出来。→ 改成**缩写白名单**（JSON / OTLP / VRAM …），其余全大写词都允许本地化。
4. **批量太大时模型会把 token 花在推理上，输出被截断**（响应里只有 thinking 块、没有 text 块）。20 条一批时偶发，改成 12 条一批就稳了。
5. **OAuth token 约 1 小时过期。** 长跑时拿着启动时的旧 token 会整片 401。→ 每批重新读一次认证文件，401 时再刷一次。
6. **闸门放宽后测试要同步放宽**，否则「译文不丢占位符」会反过来把正确的译文判不合格（要求保留 DISCOURAGED 这种词）。
---

## 密钥识别踩过的坑

1. **`token` 在 AI 配置里是双义词**：既指凭证（`apiKey`、`authToken`），也指计数（`maxTokens`）。→ 分三级匹配：强信号（`password`/`secret`/`credential`）整段命中、复合信号（`api_key`）连续分段命中、弱信号（`token`/`auth`）必须整段就是这个词。`maxTokens` / `tokenizer` / `authorization` 都不会被误伤。
2. **掩码值不能写回文件。** 用户没碰过的密钥字段在编辑器里显示的是掩码，如果直接写回就把配置改坏了。→ `apply_edits` 识别「密钥路径 + 掩码值」并跳过该操作，保留原值。
3. **原始文本不能发给前端。** 原文里有明文密钥。→ `open_file` 不返回 `text` 字段，只返回脱敏后的结构化 `value`。

---

## 校验器在本机抓到的真实问题

```
✕ permissions.allow[3] — 不匹配要求的格式 /^((Agent|Artifact|Bash|...|Write)(\([^)]+\))?|mcp__.*)$/
✕ permissions.allow[7] — 不匹配要求的格式 ...
⚠ effort               — schema 中不存在该配置项
⚠ autoCompactThreshold — schema 中不存在该配置项
```

- `permissions.allow[3]` 的值是 `"ead"` —— **少了个 R 的笔误**，权限规则实际不会生效
- `permissions.allow[7]` 的值是 `"Bash(node *)*"` —— **多了个星号**
- `effort` / `autoCompactThreshold`：schema 里只有 `effortLevel` / `autoCompactEnabled`，**schema 与本机 Claude Code 版本不一致** —— 这是「schema 时效性」风险的活样本

---

## 测试

```bash
cargo test --release      # 67 个测试
```

- `tests/edit_roundtrip.rs` —— 32 个保格式编辑回归测试：改值只动该动的字节、注释不丢、键顺序不变、删完仍是合法 JSON/TOML/YAML、数组插入/移动顺序正确、掩码值不会被写回、**父级不存在时也能新增嵌套键**
- `tests/secret_test.rs` —— 8 个密钥测试：该判成密钥的都判出来、`maxTokens` 这类不误伤、脱敏后明文不出现、掩码可可靠识别
- `tests/contract.rs` —— 9 个前端 ↔ Rust 的 JSON 契约测试：锁住前端读到的每个字段名，以及前端发出的载荷能正确反序列化
- `tests/scan_test.rs` —— 7 个扫描器回归测试：项目发现、全局配置目录不算项目、通用文件名的归属判定、结果上限不截断项目列表
- `tests/i18n_test.rs` —— 6 个翻译质量测试：词条与 IR 里的键**完全对齐**（不漏一条）、key 格式统一、占位符/标识符零丢失、译文确实被翻了、术语表里的词已本地化
- `tests/schema_sources_test.rs` —— 5 个 schema 来源测试：每家工具都有来源、本地 schema 文件齐全且合法、确实被编译进二进制、缓存文件名不互相覆盖、工具 id 不重复

界面层用 HTTP 桥 + Playwright 做过端到端验证（4 套）：

- **v1 回归**：配置字典 → 扫描磁盘（项目分组）→ 点「编辑」进入独立编辑子页面 → 改值 → 校验 → 差异预览 → 保存 → 核对文件确实落盘且备份已生成，控制台零错误
- **v2 回归**：「已配置」标记与筛选 → **全量中文 + 原文对照可展开** → 密钥显示为掩码 → 点「显示明文」拿到真值 → 新增键 → 撤销/重做 → 数组添加/上移/下移/删除 → 差异预览 → 保存落盘核对 → **外部改动冲突三选一** → 新建配置文件，控制台零错误
- **v2.1 回归**：编辑器树里每个 key 都有中文说明 → 新增键面板列出带说明的候选 → 搜索过滤 → 选中后详情区出现完整说明与**默认值占位**（`默认值：30`）→ 枚举键给出**可筛选候选列表**并能选中 → 添加后键进树且值正确 → 数组元素面板同样带说明与占位
- **v3 回归（新增工具）**：侧栏 9 家工具全部有内容 → Codex / Grok Build / Hermes / Pi 的字典里能查到真实键 → 用派生 schema 的 **Grok Build** 配置打开编辑器，树里每个键都有中文说明 → 新增键面板能列出该工具的 schema 键（带中文说明）→ 选中嵌套键 `sandbox.auto_allow_bash` 后添加，**父级不存在时自动建出来**并出现在树里

四套 E2E 都只操作 `/tmp` 下的副本；事后核对用户真实配置（`~/.claude/settings.json`、`~/.codex/config.toml` 等）**逐字节未变**。

---

## 已知限制

- **Codex 官网 schema 被 Cloudflare 挡**：`developers.openai.com/codex/config-schema.json` 在本环境返回 403；改用官方仓库 `openai/codex` 里的同一份 `codex-rs/core/config.schema.json`，内容等价且可直连。
- **5 家是派生 schema，不是官方机器可读的**：Grok Build / Pi / Hermes / OpenClaw / MiniMax Code 的键位、类型、默认值来自各自官方来源（文档字段表 / 源码定义 / 官方 npm 包），**真实但可能随上游更新而滞后** —— 更新后重跑 `tools/derive_schema.py` 即可。
- **OpenClaw 与 MiniMax Code 的说明很少**：这两家的字段文档只存在于散文里，官方包里没有字段说明表。我们不拿猜测去凑，说明只给能确证的部分（OpenClaw 30 条、MiniMax Code 0 条），其余键有类型/枚举/默认值但没有文字解释。
- **`anyOf` 未做逐分支求值**：只做「属性并集、required 不合并」的保守处理，不会误报，但也查不出缺字段。
- **YAML 只做行级定点替换**：块标量、锚点（anchor）、多行字符串等复杂结构暂不支持原地改值。
- **TOML 数组操作只支持「表 → 数组」一层下钻**：数组元素内部再嵌套（内联表）暂不支持，会明确报错而不是静默写错位置。
- **macOS 受 TCC 保护的目录扫不到**：`~/Library` 下部分目录会被系统拒绝访问，扫描时静默跳过（不报错）。
- **密钥不写钥匙串**：目前只做显示脱敏，配置里的密钥仍是明文存着。写回系统钥匙串尚未实现。
- **翻译是机器翻译**：术语表保证了术语一致，但长句偶有生硬；上游 schema 更新后需重跑 `tools/translate.py`（增量，只重译新增/变更的文本）。

---

## 下一步（按优先级）

1. 给「无精确文档链接」的键做降级策略（文档索引页 + 页内搜索）
2. 密钥写系统钥匙串（macOS Keychain）
3. OpenClaw / MiniMax Code 补字段说明（需要找到它们官方发布的字段文档或 schema）
4. 跨平台打包（Windows msi / Linux deb、AppImage）
5. 编辑器支持多文件批量改
6. 应用体验：托盘常驻、全局快捷键、深色模式、配置片段库

> 九家工具的 schema 已全部接上（4 家官方发布 + 5 家从官方来源派生），
> 3043 条带说明的键 100% 有中文译文。
