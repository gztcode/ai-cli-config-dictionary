//! cfg-explorer 核心库。
//!
//! 与 GUI 解耦：CLI、HTML 预览、未来的 Tauri/egui 桌面端都消费这里的同一套 API。
//!
//! - [`registry`]：各家工具的注册信息与路径解析
//! - [`schema`]：JSON Schema → IR 转换器
//! - [`ir`]：中间表示（GUI 的唯一数据契约）
//! - [`config`]：JSON / TOML / YAML 统一读取
//! - [`validate`]：基于 IR 的配置校验
//! - [`preview`]：HTML 预览生成
//! - [`scan`]：磁盘扫描，找出各家 AI CLI 的配置文件
//! - [`edit`]：保格式编辑（改完只动用户碰过的那几个字节）
//! - [`jsonc`]：自写的 JSON/JSONC 解析器（带字节偏移）
//! - [`secret`]：密钥识别与脱敏
//! - [`commands`]：与前端通信的命令实现（桌面 App 与 HTTP 桥共用）
//! - [`serve`]：把同一套命令暴露成 HTTP，便于脚本化与端到端验证

pub mod app;
pub mod commands;
pub mod config;
pub mod edit;
pub mod ir;
pub mod jsonc;
pub mod preview;
pub mod registry;
pub mod scan;
pub mod schema;
pub mod secret;
pub mod serve;
pub mod snippets;
pub mod validate;

pub use app::{build_ir, build_payload, AppPaths};
pub use ir::{Ir, IrKey, IrStats, SchemaSource, ValueType};
pub use preview::PreviewData;
pub use registry::{now_iso, sha256_hex, Registry, Tool};
pub use scan::{ScanOptions, ScanResult};
