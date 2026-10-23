//! 扫描器的回归测试：项目发现、工具归属、扫描范围。

use cfg_explorer::scan::{self, MatchBy, ScanOptions};
use std::path::Path;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cfg-scan-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, body: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

// ── 项目发现 ──────────────────────────────────────────

#[test]
fn 能发现项目并把配置归到项目下() {
    let d = tmp_dir("projects");
    // 一个像样的项目：有 .git 和项目级 .claude 配置
    write(&d.join("my-app/.git/HEAD"), "ref: refs/heads/main\n");
    write(
        &d.join("my-app/.claude/settings.json"),
        "{\"model\":\"x\"}\n",
    );
    // 一个只有 .claude 的项目
    write(
        &d.join("only-claude/.claude/settings.json"),
        "{\"model\":\"y\"}\n",
    );
    // 一个跟项目无关的散落配置（不在任何 AI CLI 配置目录里，不该命中）
    write(&d.join("loose/settings.json"), "{\"model\":\"z\"}\n");

    let opts = ScanOptions {
        roots: vec![d.display().to_string()],
        detect_projects: true,
        ..Default::default()
    };
    let r = scan::scan(&opts, &scan::Cancel::new());

    let roots: Vec<&str> = r.projects.iter().map(|p| p.root.as_str()).collect();
    assert!(
        roots.iter().any(|x| x.ends_with("my-app")),
        "应发现 my-app，实际: {roots:?}"
    );
    assert!(
        roots.iter().any(|x| x.ends_with("only-claude")),
        "只有 .claude 的目录也该算项目，实际: {roots:?}"
    );
    // 散落的 settings.json 所在目录没有标志文件，不该算项目
    assert!(
        !roots.iter().any(|x| x.ends_with("/loose")),
        "没有标志文件的目录不该算项目，实际: {roots:?}"
    );

    // 项目级配置应归属到项目
    let in_project: Vec<&str> = r
        .files
        .iter()
        .filter_map(|f| f.project.as_deref())
        .collect();
    assert!(
        in_project.iter().any(|x| x.ends_with("my-app")),
        "my-app 的配置应归属到项目，实际: {in_project:?}"
    );

    // 项目内配置数统计正确
    let my_app = r
        .projects
        .iter()
        .find(|p| p.root.ends_with("my-app"))
        .unwrap();
    assert_eq!(my_app.config_count, 1);
    assert!(my_app.markers.contains(&".git".to_string()));
}

#[test]
fn 全局配置目录不会被算成项目() {
    // 直接扫 ~/.claude 这种配置目录：里面的一切都是全局配置，不该冒出项目
    let base = tmp_dir("global");
    let d = base.join(".claude");
    write(&d.join("settings.json"), "{\"model\":\"x\"}\n");
    write(&d.join("plugins/x/package.json"), "{}\n");
    write(&d.join("plugins/x/.mcp.json"), "{}\n");

    let opts = ScanOptions {
        roots: vec![d.display().to_string()],
        detect_projects: true,
        ..Default::default()
    };
    let r = scan::scan(&opts, &scan::Cancel::new());

    assert!(
        r.projects.is_empty(),
        "全局配置目录内部不该算出项目，实际: {:?}",
        r.projects.iter().map(|p| &p.root).collect::<Vec<_>>()
    );
    // 里面的配置也不该归属到任何项目
    for f in &r.files {
        assert!(
            f.project.is_none(),
            "全局配置不该归属到项目: {} -> {:?}",
            f.path,
            f.project
        );
    }
}

// ── 工具归属 ──────────────────────────────────────────

#[test]
fn 工具归属按路径段判断而不是全路径子串() {
    // 项目里的 .claude 目录 —— 是 Claude Code
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/proj/.claude/settings.json")).as_deref(),
        Some("Claude Code")
    );
    // .claude 旁边的另一个配置 —— 不该被误判（这才是原 bug）
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/proj/openspec/config.yaml")),
        None,
        "只是旁边有 .claude 目录，不代表这个文件属于 Claude Code"
    );
    // 位于 .claude 目录树内的，仍算 Claude Code（最近的工具目录祖先）
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/proj/.claude/cc-haha/settings.json")).as_deref(),
        Some("Claude Code")
    );
    // 其它工具
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/.codex/config.toml")).as_deref(),
        Some("Codex CLI")
    );
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/.config/opencode/opencode.json")).as_deref(),
        Some("OpenCode")
    );
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/.pi/agent/models.json")).as_deref(),
        Some("Pi")
    );
    assert_eq!(
        scan::guess_tool(Path::new("/Users/x/.minimax/config.yaml")).as_deref(),
        Some("MiniMax Code")
    );
}

// ── 扫描范围 ──────────────────────────────────────────

#[test]
fn 扫描范围_用户目录与全盘() {
    let home = cfg_explorer::commands::scan_roots("home");
    assert_eq!(home, vec!["~".to_string()]);

    let disk = cfg_explorer::commands::scan_roots("disk");
    assert_eq!(disk, vec!["/".to_string()]);

    let custom = cfg_explorer::commands::scan_roots("custom");
    assert!(custom.is_empty());
}

#[test]
fn 常见项目目录_存在才返回() {
    let roots = scan::common_project_roots();
    // 只返回真实存在的目录
    for r in &roots {
        assert!(Path::new(r).is_dir(), "不存在的目录不该被返回: {r}");
    }
}

// ── 匹配 ──────────────────────────────────────────────

#[test]
fn 已知文件名与内容特征都能命中() {
    let d = tmp_dir("match");
    // 放在 .claude 配置目录里 —— 通用文件名只有在这里才算命中
    write(&d.join(".claude/settings.json"), "{\"model\":\"x\"}\n");
    // 文件名不认识，但内容里有特征串
    write(
        &d.join("weird-name.json"),
        "{\"base\":\"ANTHROPIC_BASE_URL=https://x\"}\n",
    );

    let opts = ScanOptions {
        roots: vec![d.display().to_string()],
        known_only: true,
        detect_projects: false,
        ..Default::default()
    };
    let r = scan::scan(&opts, &scan::Cancel::new());
    assert!(
        r.files.iter().any(|f| f.path.ends_with("settings.json")),
        "已知文件名应命中"
    );
    assert!(
        !r.files.iter().any(|f| f.path.ends_with("weird-name.json")),
        "只匹配已知文件名时，weird-name.json 不该命中"
    );

    // 打开内容特征通道后应能命中
    let opts = ScanOptions {
        roots: vec![d.display().to_string()],
        known_only: false,
        detect_projects: false,
        ..Default::default()
    };
    let r = scan::scan(&opts, &scan::Cancel::new());
    let hit = r.files.iter().find(|f| f.path.ends_with("weird-name.json"));
    assert!(hit.is_some(), "内容特征应命中");
    assert_eq!(hit.unwrap().matched_by, MatchBy::Content);
}

#[test]
fn 结果达到上限后仍继续遍历_项目列表不被截断() {
    let d = tmp_dir("limit");
    // 造 30 个项目，每个一个配置文件
    for i in 0..30 {
        write(&d.join(format!("p{i}/.git/HEAD")), "ref\n");
        write(&d.join(format!("p{i}/.claude/settings.json")), "{}\n");
    }
    let opts = ScanOptions {
        roots: vec![d.display().to_string()],
        limit: 3, // 文件结果只收 3 个
        detect_projects: true,
        ..Default::default()
    };
    let r = scan::scan(&opts, &scan::Cancel::new());
    assert_eq!(r.files.len(), 3, "文件结果应被 limit 截断");
    assert!(r.truncated, "应标记为已截断");
    assert_eq!(
        r.projects.len(),
        30,
        "项目列表不该被文件结果的 limit 截断，实际 {}",
        r.projects.len()
    );
}
