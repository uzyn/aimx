//! Per-agent plugin installer for `aimx agent-setup`.
//!
//! Ships plugin/skill packages for supported agents (currently Claude Code)
//! bundled into the binary via `include_dir!`, and installs them into the
//! user's `$HOME`-based agent directory.

use crate::term;
use include_dir::{Dir, DirEntry, include_dir};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

static AGENTS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/agents");

/// Short name and install metadata for a supported agent.
pub struct AgentSpec {
    /// Registry key passed on the CLI (e.g. `claude-code`).
    pub name: &'static str,
    /// Path inside the `agents/` tree that holds the plugin source.
    pub source_subdir: &'static str,
    /// Destination template, with `$HOME` / `$XDG_CONFIG_HOME` placeholders.
    pub dest_template: &'static str,
    /// Renders the post-install message. Receives the effective data
    /// directory (when the user passed `--data-dir`) so agents that need
    /// the user to paste a JSON/JSONC snippet can embed the right
    /// `--data-dir` argument into that snippet.
    pub activation_hint: fn(data_dir: Option<&Path>) -> String,
}

/// Static registry of supported agents.
pub fn registry() -> &'static [AgentSpec] {
    &[
        AgentSpec {
            name: "claude-code",
            source_subdir: "claude-code",
            dest_template: "$HOME/.claude/plugins/aimx",
            activation_hint: claude_code_hint,
        },
        AgentSpec {
            name: "codex",
            source_subdir: "codex",
            dest_template: "$HOME/.codex/plugins/aimx",
            activation_hint: codex_hint,
        },
        AgentSpec {
            name: "opencode",
            source_subdir: "opencode",
            dest_template: "$XDG_CONFIG_HOME/opencode/skills/aimx",
            activation_hint: opencode_hint,
        },
        AgentSpec {
            name: "gemini",
            source_subdir: "gemini",
            dest_template: "$HOME/.gemini/skills/aimx",
            activation_hint: gemini_hint,
        },
    ]
}

fn claude_code_hint(_data_dir: Option<&Path>) -> String {
    "Plugin installed. Restart Claude Code to pick it up (it is auto-discovered from ~/.claude/plugins/).".to_string()
}

fn codex_hint(_data_dir: Option<&Path>) -> String {
    "Plugin installed. Restart Codex CLI to pick it up (it is auto-discovered from ~/.codex/plugins/).".to_string()
}

fn opencode_hint(data_dir: Option<&Path>) -> String {
    let command_array = match data_dir {
        Some(dd) => format!(
            "[\"/usr/local/bin/aimx\", \"--data-dir\", \"{}\", \"mcp\"]",
            dd.display()
        ),
        None => "[\"/usr/local/bin/aimx\", \"mcp\"]".to_string(),
    };
    format!(
        "Skill installed. Add the following block to the `mcp` object in \
         your OpenCode config (~/.config/opencode/opencode.json or \
         <repo>/opencode.json), then restart OpenCode:\n\
         \n\
         {{\n\
         \x20\x20\"mcp\": {{\n\
         \x20\x20\x20\x20\"aimx\": {{\n\
         \x20\x20\x20\x20\x20\x20\"command\": {command_array}\n\
         \x20\x20\x20\x20}}\n\
         \x20\x20}}\n\
         }}"
    )
}

fn gemini_hint(data_dir: Option<&Path>) -> String {
    let args_line = match data_dir {
        Some(dd) => format!(
            "\x20\x20\x20\x20\x20\x20\"args\": [\"--data-dir\", \"{}\", \"mcp\"]",
            dd.display()
        ),
        None => "\x20\x20\x20\x20\x20\x20\"args\": [\"mcp\"]".to_string(),
    };
    format!(
        "Skill installed. Merge the following block into \
         ~/.gemini/settings.json (create the file if it does not exist), \
         then restart Gemini CLI:\n\
         \n\
         {{\n\
         \x20\x20\"mcpServers\": {{\n\
         \x20\x20\x20\x20\"aimx\": {{\n\
         \x20\x20\x20\x20\x20\x20\"command\": \"/usr/local/bin/aimx\",\n\
         {args_line}\n\
         \x20\x20\x20\x20}}\n\
         \x20\x20}}\n\
         }}"
    )
}

pub fn find_agent(name: &str) -> Option<&'static AgentSpec> {
    registry().iter().find(|a| a.name == name)
}

/// Trait used to make installs testable without touching the real `$HOME`
/// or real uid.
pub trait AgentEnv {
    fn home_dir(&self) -> Option<PathBuf>;
    fn xdg_config_home(&self) -> Option<PathBuf>;
    fn is_root(&self) -> bool;
    fn is_stdin_tty(&self) -> bool;
    fn read_line(&self) -> io::Result<String>;
}

pub struct RealAgentEnv;

impl AgentEnv for RealAgentEnv {
    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn xdg_config_home(&self) -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
    }

    fn is_root(&self) -> bool {
        // SAFETY: libc::geteuid is a simple syscall with no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    fn is_stdin_tty(&self) -> bool {
        use std::io::IsTerminal;
        io::stdin().is_terminal()
    }

    fn read_line(&self) -> io::Result<String> {
        let mut s = String::new();
        io::stdin().lock().read_line(&mut s)?;
        Ok(s)
    }
}

/// Options controlling a single `agent-setup` invocation.
pub struct InstallOptions<'a> {
    pub force: bool,
    pub print: bool,
    pub data_dir: Option<&'a Path>,
}

/// Resolve a destination template against the environment. Substitutes
/// `$HOME` and `$XDG_CONFIG_HOME`.
pub fn resolve_dest(template: &str, env: &dyn AgentEnv) -> Result<PathBuf, String> {
    let home = env.home_dir().ok_or_else(|| {
        "HOME is not set; agent-setup writes to the user's home directory".to_string()
    })?;

    let xdg = env
        .xdg_config_home()
        .unwrap_or_else(|| home.join(".config"));

    let substituted = template
        .replace("$XDG_CONFIG_HOME", &xdg.to_string_lossy())
        .replace("$HOME", &home.to_string_lossy());

    Ok(PathBuf::from(substituted))
}

/// Entry point called from `main.rs`.
pub fn run(
    agent: Option<String>,
    list: bool,
    force: bool,
    print: bool,
    data_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = RealAgentEnv;
    run_with_env(agent, list, force, print, data_dir, &env)
}

pub fn run_with_env(
    agent: Option<String>,
    list: bool,
    force: bool,
    print: bool,
    data_dir: Option<&Path>,
    env: &dyn AgentEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    if list {
        print_registry(env);
        return Ok(());
    }

    let name = agent.ok_or_else(|| {
        "agent-setup requires an agent name, or --list to see supported agents".to_string()
    })?;

    if env.is_root() {
        return Err("agent-setup is a per-user operation — run without sudo or as root".into());
    }

    let spec = find_agent(&name).ok_or_else(|| {
        format!("unknown agent '{name}'; run `aimx agent-setup --list` to see supported agents")
    })?;

    let opts = InstallOptions {
        force,
        print,
        data_dir,
    };

    install(spec, &opts, env)
}

fn print_registry(env: &dyn AgentEnv) {
    println!("{}", term::header("Supported agents:"));
    println!();
    for spec in registry() {
        let dest = resolve_dest(spec.dest_template, env)
            .unwrap_or_else(|_| PathBuf::from(spec.dest_template));
        println!("  {}", term::highlight(spec.name));
        println!("    destination: {}", dest.display());
        println!("    install:     aimx agent-setup {}", spec.name);
        let hint = (spec.activation_hint)(None);
        let mut hint_lines = hint.lines();
        if let Some(first) = hint_lines.next() {
            println!("    activation:  {first}");
            for line in hint_lines {
                println!("                 {line}");
            }
        }
        println!();
    }
}

fn install(
    spec: &AgentSpec,
    opts: &InstallOptions,
    env: &dyn AgentEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = AGENTS_DIR.get_dir(spec.source_subdir).ok_or_else(|| {
        format!(
            "internal error: missing embedded source for '{}'",
            spec.name
        )
    })?;

    let files = assemble_plugin_files(source, opts.data_dir)?;
    let hint = (spec.activation_hint)(opts.data_dir);

    if opts.print {
        for (rel, bytes) in &files {
            println!("=== {} ===", rel.display());
            match std::str::from_utf8(bytes) {
                Ok(text) => println!("{text}"),
                Err(_) => println!("<{} bytes of binary content>", bytes.len()),
            }
        }
        // `--print` also emits the activation hint so snippet-style agents
        // (opencode, gemini) expose their MCP JSON block under dry-run.
        println!("=== activation ===");
        println!("{hint}");
        return Ok(());
    }

    let dest_root = resolve_dest(spec.dest_template, env)?;
    write_files(&dest_root, &files, opts.force, env)?;

    println!(
        "{} {}",
        term::success("Installed"),
        term::highlight(&dest_root.to_string_lossy())
    );
    println!("{hint}");

    Ok(())
}

/// Walk the embedded plugin source, transform known files (skill header +
/// primer, plugin.json), and return the full set of files to write.
///
/// Returns `(relative_path, bytes)` pairs. Relative paths are relative to
/// the install destination root.
pub fn assemble_plugin_files(
    source: &Dir<'_>,
    data_dir: Option<&Path>,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    collect_entries(source, Path::new(""), &mut out)?;

    // Transformation 1: assemble SKILL.md from SKILL.md.header + common primer.
    let primer = AGENTS_DIR
        .get_file("common/aimx-primer.md")
        .ok_or_else(|| {
            "internal error: missing common/aimx-primer.md in embedded assets".to_string()
        })?
        .contents();

    let mut transformed: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for (rel, bytes) in out.drain(..) {
        if rel.file_name().is_some_and(|n| n == "SKILL.md.header") {
            let target = rel.with_file_name("SKILL.md");
            let mut combined = bytes.clone();
            combined.extend_from_slice(primer);
            transformed.push((target, combined));
            continue;
        }

        if rel.file_name().is_some_and(|n| n == "plugin.json")
            && let Some(dd) = data_dir
        {
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| format!("plugin.json not valid UTF-8: {e}"))?;
            let rewritten = rewrite_plugin_args(text, dd)?;
            transformed.push((rel, rewritten.into_bytes()));
            continue;
        }

        // Skip README.md at the top of the plugin source — it is developer-facing,
        // not an artifact to install.
        //
        // NOTE: this match is deliberately top-level only (`rel.as_os_str()`
        // rather than `rel.file_name()`), so a nested file such as
        // `docs/README.md` inside a plugin tree would still be installed.
        // Keep this scoping if you touch the filter.
        if rel.as_os_str() == "README.md" {
            continue;
        }

        transformed.push((rel, bytes));
    }

    transformed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(transformed)
}

fn collect_entries(
    dir: &Dir<'_>,
    rel_root: &Path,
    out: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<(), String> {
    for entry in dir.entries() {
        match entry {
            DirEntry::File(f) => {
                let rel = f
                    .path()
                    .strip_prefix(dir.path())
                    .map_err(|e| format!("strip_prefix failed: {e}"))?;
                out.push((rel_root.join(rel), f.contents().to_vec()));
            }
            DirEntry::Dir(sub) => {
                let rel = sub
                    .path()
                    .strip_prefix(dir.path())
                    .map_err(|e| format!("strip_prefix failed: {e}"))?;
                collect_entries(sub, &rel_root.join(rel), out)?;
            }
        }
    }
    Ok(())
}

/// Rewrite `mcpServers.<server>.args` in a plugin.json-like JSON so that the
/// command runs with `--data-dir <path>`. Preserves other fields.
///
/// Implementation parses the JSON into `serde_json::Value`, swaps the `args`
/// array on each server entry, and re-serializes via `to_string_pretty`. The
/// output is therefore serde-formatted, not byte-identical to the hand-authored
/// file — acceptable because `plugin.json` has no comments or meaningful
/// whitespace to preserve.
pub fn rewrite_plugin_args(json_text: &str, data_dir: &Path) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| format!("plugin.json is not valid JSON: {e}"))?;

    let mut value = value;
    let dd = data_dir.to_string_lossy().into_owned();

    if let Some(servers) = value.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        for (_key, server) in servers.iter_mut() {
            if let Some(obj) = server.as_object_mut() {
                obj.insert(
                    "args".to_string(),
                    serde_json::json!(["--data-dir", dd, "mcp"]),
                );
            }
        }
    }

    serde_json::to_string_pretty(&value)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| format!("failed to serialize plugin.json: {e}"))
}

fn write_files(
    dest_root: &Path,
    files: &[(PathBuf, Vec<u8>)],
    force: bool,
    env: &dyn AgentEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check for existing files first so we prompt once, not per file.
    if !force {
        let existing: Vec<&Path> = files
            .iter()
            .map(|(rel, _)| rel.as_path())
            .filter(|rel| dest_root.join(rel).exists())
            .collect();

        if !existing.is_empty() {
            if !env.is_stdin_tty() {
                return Err(format!(
                    "destination files already exist under {}; pass --force to overwrite",
                    dest_root.display()
                )
                .into());
            }

            println!(
                "{} Destination {} already contains {} file(s):",
                term::warn("Warning:"),
                dest_root.display(),
                existing.len()
            );
            for rel in &existing {
                println!("  {}", rel.display());
            }
            print!("Overwrite? [y/N] ");
            io::stdout().flush().ok();
            let line = env.read_line()?;
            if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
                return Err("aborted by user".into());
            }
        }
    }

    for (rel, bytes) in files {
        let full = dest_root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
            set_dir_mode(parent)?;
        }
        std::fs::write(&full, bytes)?;
        set_file_mode(&full)?;
    }

    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(path, perms)
}

#[cfg(unix)]
fn set_dir_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_file_mode(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::TempDir;

    struct MockEnv {
        home: PathBuf,
        xdg: Option<PathBuf>,
        root: bool,
        tty: bool,
        responses: RefCell<Vec<String>>,
    }

    impl MockEnv {
        fn new(home: PathBuf) -> Self {
            Self {
                home,
                xdg: None,
                root: false,
                tty: false,
                responses: RefCell::new(Vec::new()),
            }
        }
    }

    impl AgentEnv for MockEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn xdg_config_home(&self) -> Option<PathBuf> {
            self.xdg.clone()
        }
        fn is_root(&self) -> bool {
            self.root
        }
        fn is_stdin_tty(&self) -> bool {
            self.tty
        }
        fn read_line(&self) -> io::Result<String> {
            Ok(self.responses.borrow_mut().remove(0))
        }
    }

    #[test]
    fn registry_contains_claude_code() {
        assert!(find_agent("claude-code").is_some());
        assert!(find_agent("not-a-real-agent").is_none());
    }

    #[test]
    fn resolve_dest_substitutes_home() {
        let env = MockEnv::new(PathBuf::from("/home/alice"));
        let path = resolve_dest("$HOME/.claude/plugins/aimx", &env).unwrap();
        assert_eq!(path, PathBuf::from("/home/alice/.claude/plugins/aimx"));
    }

    #[test]
    fn resolve_dest_substitutes_xdg_config_home() {
        let mut env = MockEnv::new(PathBuf::from("/home/alice"));
        env.xdg = Some(PathBuf::from("/alt/config"));
        let path = resolve_dest("$XDG_CONFIG_HOME/foo", &env).unwrap();
        assert_eq!(path, PathBuf::from("/alt/config/foo"));
    }

    #[test]
    fn resolve_dest_defaults_xdg_to_home_dot_config() {
        let env = MockEnv::new(PathBuf::from("/home/alice"));
        let path = resolve_dest("$XDG_CONFIG_HOME/foo", &env).unwrap();
        assert_eq!(path, PathBuf::from("/home/alice/.config/foo"));
    }

    #[test]
    fn install_claude_code_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        assert!(dest.join(".claude-plugin/plugin.json").exists());
        assert!(dest.join("skills/aimx/SKILL.md").exists());
        // README.md is developer-facing and is NOT installed.
        assert!(!dest.join("README.md").exists());

        // SKILL.md should have been assembled from header + primer.
        let skill = std::fs::read_to_string(dest.join("skills/aimx/SKILL.md")).unwrap();
        assert!(
            skill.starts_with("---\n"),
            "missing YAML frontmatter: {skill:.200}"
        );
        assert!(skill.contains("name: aimx"));
        assert!(skill.contains("MCP tools"));
        assert!(skill.contains("mailbox_create"));
        assert!(skill.contains("Trust model"));
        // The template sentinel should NOT appear on disk.
        assert!(!dest.join("skills/aimx/SKILL.md.header").exists());

        // plugin.json should have default args (no --data-dir).
        let plugin = std::fs::read_to_string(dest.join(".claude-plugin/plugin.json")).unwrap();
        assert!(plugin.contains("\"mcp\""));
        assert!(!plugin.contains("--data-dir"));
    }

    #[test]
    fn install_refuses_root() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        env.root = true;

        let err =
            run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap_err();
        assert!(err.to_string().contains("per-user"));
    }

    #[test]
    fn install_unknown_agent_errors() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let err = run_with_env(Some("bogus".into()), false, false, false, None, &env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown agent"));
        assert!(msg.contains("--list"));
    }

    #[test]
    fn install_refuses_to_overwrite_without_force_non_tty() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        // First install succeeds.
        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        // Second install without --force on non-TTY errors.
        let err =
            run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap_err();
        assert!(err.to_string().contains("--force"));
    }

    #[test]
    fn install_force_overwrites() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();
        run_with_env(Some("claude-code".into()), false, true, false, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        assert!(dest.join(".claude-plugin/plugin.json").exists());
    }

    #[test]
    fn install_print_writes_no_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("claude-code".into()), false, false, true, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        assert!(!dest.exists());
    }

    #[test]
    fn install_with_custom_data_dir_rewrites_plugin_args() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let custom = PathBuf::from("/custom/aimx-data");

        run_with_env(
            Some("claude-code".into()),
            false,
            false,
            false,
            Some(&custom),
            &env,
        )
        .unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        let plugin = std::fs::read_to_string(dest.join(".claude-plugin/plugin.json")).unwrap();
        assert!(plugin.contains("--data-dir"));
        assert!(plugin.contains("/custom/aimx-data"));
        assert!(plugin.contains("\"mcp\""));
    }

    #[test]
    fn list_mode_runs_without_agent_name() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        run_with_env(None, true, false, false, None, &env).unwrap();
    }

    #[test]
    fn missing_agent_name_errors_when_not_listing() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let err = run_with_env(None, false, false, false, None, &env).unwrap_err();
        assert!(err.to_string().contains("agent name"));
    }

    #[test]
    fn install_tty_prompt_yes_overwrites() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());

        // First install succeeds.
        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        // Second install: TTY says "y", overwrite proceeds.
        env.tty = true;
        env.responses = RefCell::new(vec!["y\n".to_string()]);
        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        assert!(dest.join(".claude-plugin/plugin.json").exists());
    }

    #[test]
    fn install_tty_prompt_no_aborts() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());

        // First install succeeds.
        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        // Second install: TTY says "n", install aborts.
        env.tty = true;
        env.responses = RefCell::new(vec!["n\n".to_string()]);
        let err =
            run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap_err();
        assert!(err.to_string().contains("aborted by user"));
    }

    #[test]
    fn assembled_skill_md_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("claude-code").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "skills/aimx/SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("claude-code/skills/aimx/SKILL.md.header")
            .unwrap()
            .contents();
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();

        let mut expected = Vec::with_capacity(header.len() + primer.len());
        expected.extend_from_slice(header);
        expected.extend_from_slice(primer);

        assert_eq!(
            skill_bytes, &expected,
            "SKILL.md must be exact concatenation of header followed by primer"
        );
    }

    #[test]
    fn rewrite_plugin_args_rejects_malformed_json() {
        let err = rewrite_plugin_args("{ not json", Path::new("/tmp/x")).unwrap_err();
        assert!(
            err.contains("plugin.json is not valid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn registry_contains_sprint_29_agents() {
        for name in ["codex", "opencode", "gemini"] {
            assert!(
                find_agent(name).is_some(),
                "registry missing sprint-29 agent: {name}"
            );
        }
    }

    #[test]
    fn install_codex_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("codex".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".codex/plugins/aimx");
        assert!(dest.join(".codex-plugin/plugin.json").exists());
        assert!(dest.join("skills/aimx/SKILL.md").exists());
        assert!(!dest.join("README.md").exists());
        assert!(!dest.join("skills/aimx/SKILL.md.header").exists());

        let skill = std::fs::read_to_string(dest.join("skills/aimx/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\n"));
        assert!(skill.contains("name: aimx"));
        assert!(skill.contains("MCP tools"));
        assert!(skill.contains("mailbox_create"));

        let plugin = std::fs::read_to_string(dest.join(".codex-plugin/plugin.json")).unwrap();
        assert!(plugin.contains("\"mcp\""));
        assert!(!plugin.contains("--data-dir"));
    }

    #[test]
    fn install_codex_with_custom_data_dir_rewrites_plugin_args() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let custom = PathBuf::from("/custom/aimx-data");

        run_with_env(
            Some("codex".into()),
            false,
            false,
            false,
            Some(&custom),
            &env,
        )
        .unwrap();

        let plugin = std::fs::read_to_string(
            tmp.path()
                .join(".codex/plugins/aimx/.codex-plugin/plugin.json"),
        )
        .unwrap();
        assert!(plugin.contains("--data-dir"));
        assert!(plugin.contains("/custom/aimx-data"));
    }

    #[test]
    fn install_opencode_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("opencode".into()), false, false, false, None, &env).unwrap();

        // XDG_CONFIG_HOME defaults to $HOME/.config when unset. OpenCode
        // discovers skills from `~/.config/opencode/skills/<name>/`.
        let dest = tmp.path().join(".config/opencode/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        // OpenCode ships a skill-only package; no plugin manifest on disk.
        assert!(!dest.join("plugin.json").exists());
        assert!(!dest.join("README.md").exists());
        assert!(!dest.join("SKILL.md.header").exists());

        let skill = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(skill.starts_with("---\n"));
        assert!(skill.contains("mailbox_create"));
    }

    #[test]
    fn install_opencode_respects_xdg_config_home_override() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        let xdg = tmp.path().join("alt-xdg");
        env.xdg = Some(xdg.clone());

        run_with_env(Some("opencode".into()), false, false, false, None, &env).unwrap();

        assert!(xdg.join("opencode/skills/aimx/SKILL.md").exists());
    }

    #[test]
    fn install_gemini_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("gemini".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".gemini/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(!dest.join("plugin.json").exists());
        assert!(!dest.join("README.md").exists());
        assert!(!dest.join("SKILL.md.header").exists());

        let skill = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(skill.starts_with("---\n"));
        assert!(skill.contains("mailbox_create"));
    }

    #[test]
    fn claude_code_activation_hint_is_short_and_deterministic() {
        let spec = find_agent("claude-code").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("Restart Claude Code"));
        assert!(hint.contains("auto-discovered"));
        // Data-dir override should not change Claude Code's hint — the MCP
        // args are baked into plugin.json, not the hint.
        assert_eq!(hint, (spec.activation_hint)(Some(Path::new("/x"))));
    }

    #[test]
    fn codex_activation_hint_mentions_codex() {
        let spec = find_agent("codex").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("Codex"));
        assert!(hint.contains("auto-discovered"));
    }

    #[test]
    fn opencode_activation_hint_embeds_jsonc_snippet() {
        let spec = find_agent("opencode").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("\"mcp\""));
        assert!(hint.contains("\"aimx\""));
        assert!(hint.contains("\"command\""));
        assert!(hint.contains("/usr/local/bin/aimx"));
        assert!(hint.contains("opencode.json"));
        // Default (no --data-dir) should not mention it.
        assert!(!hint.contains("--data-dir"));

        let hint_custom = (spec.activation_hint)(Some(Path::new("/custom/data")));
        assert!(hint_custom.contains("--data-dir"));
        assert!(hint_custom.contains("/custom/data"));
    }

    #[test]
    fn gemini_activation_hint_embeds_mcp_servers_snippet() {
        let spec = find_agent("gemini").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("\"mcpServers\""));
        assert!(hint.contains("\"aimx\""));
        assert!(hint.contains("settings.json"));
        assert!(hint.contains("/usr/local/bin/aimx"));
        assert!(!hint.contains("--data-dir"));

        let hint_custom = (spec.activation_hint)(Some(Path::new("/var/aimx")));
        assert!(hint_custom.contains("--data-dir"));
        assert!(hint_custom.contains("/var/aimx"));
    }

    #[test]
    fn opencode_print_emits_activation_snippet() {
        // --print should dump both the file tree and the activation hint so
        // snippet-style agents surface their JSON block under dry-run.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        // We can't easily capture stdout here without adding infrastructure,
        // so assert --print writes no files and succeeds for all snippet
        // agents (the hint content itself is covered by the activation_hint
        // tests above).
        run_with_env(Some("opencode".into()), false, false, true, None, &env).unwrap();
        assert!(!tmp.path().join(".config/opencode").exists());

        run_with_env(Some("gemini".into()), false, false, true, None, &env).unwrap();
        assert!(!tmp.path().join(".gemini").exists());
    }

    #[test]
    fn assembled_opencode_skill_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("opencode").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("opencode/SKILL.md.header")
            .unwrap()
            .contents();
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();

        let mut expected = Vec::with_capacity(header.len() + primer.len());
        expected.extend_from_slice(header);
        expected.extend_from_slice(primer);

        assert_eq!(skill_bytes, &expected);
    }

    #[test]
    fn assembled_gemini_skill_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("gemini").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("gemini/SKILL.md.header")
            .unwrap()
            .contents();
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();

        let mut expected = Vec::with_capacity(header.len() + primer.len());
        expected.extend_from_slice(header);
        expected.extend_from_slice(primer);

        assert_eq!(skill_bytes, &expected);
    }

    #[test]
    fn assembled_codex_skill_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("codex").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "skills/aimx/SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("codex/skills/aimx/SKILL.md.header")
            .unwrap()
            .contents();
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();

        let mut expected = Vec::with_capacity(header.len() + primer.len());
        expected.extend_from_slice(header);
        expected.extend_from_slice(primer);

        assert_eq!(skill_bytes, &expected);
    }

    #[test]
    fn registry_lists_four_agents_in_canonical_order() {
        let names: Vec<&str> = registry().iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["claude-code", "codex", "opencode", "gemini"]);
    }

    #[cfg(unix)]
    #[test]
    fn install_sets_expected_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        let plugin = dest.join(".claude-plugin/plugin.json");
        let mode = std::fs::metadata(&plugin).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "plugin.json should be 0o644, got {mode:o}");

        let plugin_dir = dest.join(".claude-plugin");
        let dmode = std::fs::metadata(&plugin_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dmode, 0o755,
            ".claude-plugin dir should be 0o755, got {dmode:o}"
        );
    }
}
