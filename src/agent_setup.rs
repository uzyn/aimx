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
    /// When `true`, the installer copies `agents/common/references/*.md`
    /// alongside the installed SKILL.md. Agents that support progressive
    /// disclosure (Claude Code, Codex, OpenClaw) load reference files on
    /// demand. Agents that take a single blob (Goose, Gemini, OpenCode)
    /// receive only the main primer.
    pub progressive_disclosure: bool,
}

/// Static registry of supported agents.
///
/// v1 roster: `claude-code`, `codex`, `opencode`, `gemini`, `goose`,
/// `openclaw`, `hermes` (PRD §6.10 FR-50). Source-tree layout asymmetry is by
/// design; `assemble_plugin_files` walks each source tree relative to its
/// root and handles all three shapes. Do not "normalize" the layout;
/// the destination template determines the depth.
///
/// Source-tree shapes:
/// - Plugin-with-skill (`claude-code`): `plugin.json` at the package
///   root with the skill nested under `skills/aimx/`, so the installed
///   tree mirrors Claude Code's plugin-manifest convention. Claude Code
///   auto-discovers plugins under `~/.claude/plugins/`.
/// - Flat skill (`codex`, `opencode`, `gemini`, `openclaw`):
///   `SKILL.md.header` at the source root; the destination template
///   points directly at the skill directory. No plugin manifest is
///   written. Codex CLI specifically does NOT scan a plugins directory
///   for MCP servers. Its MCP wiring lives in `~/.codex/config.toml`
///   (managed via `codex mcp add`), so the activation hint prints the
///   canonical `codex mcp add aimx -- ...` command for the user.
/// - Flat recipe (`goose`): `aimx.yaml.header` at the source root; the
///   installer concatenates `<name>.yaml.header` with the indented common
///   primer to produce a single `<name>.yaml` file under the Goose
///   recipes directory.
pub fn registry() -> &'static [AgentSpec] {
    &[
        AgentSpec {
            name: "claude-code",
            source_subdir: "claude-code",
            dest_template: "$HOME/.claude/plugins/aimx",
            activation_hint: claude_code_hint,
            progressive_disclosure: true,
        },
        AgentSpec {
            name: "codex",
            source_subdir: "codex",
            dest_template: "$HOME/.codex/skills/aimx",
            activation_hint: codex_hint,
            progressive_disclosure: true,
        },
        AgentSpec {
            name: "opencode",
            source_subdir: "opencode",
            dest_template: "$XDG_CONFIG_HOME/opencode/skills/aimx",
            activation_hint: opencode_hint,
            progressive_disclosure: false,
        },
        AgentSpec {
            name: "gemini",
            source_subdir: "gemini",
            dest_template: "$HOME/.gemini/skills/aimx",
            activation_hint: gemini_hint,
            progressive_disclosure: false,
        },
        AgentSpec {
            name: "goose",
            source_subdir: "goose",
            // Goose discovers recipes by filename stem from
            // ~/.config/goose/recipes/. We install one file, not a
            // directory. Destination template points at the file itself.
            dest_template: "$XDG_CONFIG_HOME/goose/recipes",
            activation_hint: goose_hint,
            progressive_disclosure: false,
        },
        AgentSpec {
            name: "openclaw",
            source_subdir: "openclaw",
            // OpenClaw scans ~/.openclaw/skills/<name>/SKILL.md. We ship a
            // skill-directory package (no flat SKILL.md at the root).
            dest_template: "$HOME/.openclaw/skills/aimx",
            activation_hint: openclaw_hint,
            progressive_disclosure: true,
        },
        AgentSpec {
            name: "hermes",
            source_subdir: "hermes",
            // Hermes Agent (Nous Research) loads skills from
            // ~/.hermes/skills/<name>/SKILL.md with optional `references/`
            // siblings. MCP servers live in ~/.hermes/config.yaml under
            // `mcp_servers:`; there is no shell-side `mcp add` CLI today, so
            // the activation hint prints a YAML snippet to paste in.
            dest_template: "$HOME/.hermes/skills/aimx",
            activation_hint: hermes_hint,
            progressive_disclosure: true,
        },
    ]
}

fn claude_code_hint(data_dir: Option<&Path>) -> String {
    // Claude Code auto-discovers plugins under ~/.claude/plugins/, but the
    // MCP server bundled with the plugin is NOT activated automatically.
    // in particular `claude -p` (headless, used by channel-trigger recipes)
    // needs an explicit `claude mcp add` to register the server in its
    // MCP registry. Finding #7 from the 2026-04-17 manual test run
    // surfaced this gap. Mirror Codex's hint structure (install-location
    // line, blank line, command line, blank line, restart note) so the
    // two agents read consistently.
    let extra_args = match data_dir {
        Some(dd) => format!(" --data-dir {}", posix_single_quote(&dd.to_string_lossy())),
        None => String::new(),
    };
    format!(
        "Plugin installed at ~/.claude/plugins/aimx/. Register the aimx MCP \
         server with Claude Code by running this command once:\n\
         \n\
         \x20\x20claude mcp add --scope user aimx /usr/local/bin/aimx{extra_args} mcp\n\
         \n\
         Restart Claude Code after registration so the new server is \
         loaded. (Claude Code auto-discovers the plugin under \
         ~/.claude/plugins/, but the MCP server must be registered \
         explicitly, especially for `claude -p` headless invocations.)"
    )
}

fn codex_hint(data_dir: Option<&Path>) -> String {
    // Codex CLI's only MCP wiring path is `~/.codex/config.toml` under
    // `[mcp_servers.<name>]`, managed via `codex mcp add`. It does NOT
    // auto-discover plugins under `~/.codex/plugins/` (validated against
    // Codex CLI 0.117.0). We install the skill file, then print the
    // canonical `codex mcp add` command for the user to run once.
    let extra_args = match data_dir {
        Some(dd) => format!(" --data-dir {}", posix_single_quote(&dd.to_string_lossy())),
        None => String::new(),
    };
    format!(
        "Skill installed at ~/.codex/skills/aimx/. Register the aimx MCP \
         server with Codex CLI by running this command once:\n\
         \n\
         \x20\x20codex mcp add aimx -- /usr/local/bin/aimx{extra_args} mcp\n\
         \n\
         Restart Codex CLI after registration so the new server is \
         loaded. (Codex CLI reads MCP servers from ~/.codex/config.toml. \
         This command updates that file for you.)"
    )
}

fn opencode_hint(data_dir: Option<&Path>) -> String {
    // Route the JSON snippet through serde_json so that a `--data-dir` path
    // containing `"` or `\` is properly escaped. `format!`-based string
    // building would produce an invalid snippet the user would copy into
    // their OpenCode config.
    let command: Vec<String> = match data_dir {
        Some(dd) => vec![
            "/usr/local/bin/aimx".to_string(),
            "--data-dir".to_string(),
            dd.to_string_lossy().into_owned(),
            "mcp".to_string(),
        ],
        None => vec!["/usr/local/bin/aimx".to_string(), "mcp".to_string()],
    };
    let snippet = serde_json::json!({
        "mcp": {
            "aimx": {
                "command": command,
            }
        }
    });
    let snippet_text = serde_json::to_string_pretty(&snippet)
        .unwrap_or_else(|_| "<failed to render snippet>".to_string());
    format!(
        "Skill installed. Add the following block to the `mcp` object in \
         your OpenCode config (~/.config/opencode/opencode.json or \
         <repo>/opencode.json), then restart OpenCode:\n\
         \n\
         {snippet_text}"
    )
}

fn gemini_hint(data_dir: Option<&Path>) -> String {
    // Route the JSON snippet through serde_json (see opencode_hint).
    let args: Vec<String> = match data_dir {
        Some(dd) => vec![
            "--data-dir".to_string(),
            dd.to_string_lossy().into_owned(),
            "mcp".to_string(),
        ],
        None => vec!["mcp".to_string()],
    };
    let snippet = serde_json::json!({
        "mcpServers": {
            "aimx": {
                "command": "/usr/local/bin/aimx",
                "args": args,
            }
        }
    });
    let snippet_text = serde_json::to_string_pretty(&snippet)
        .unwrap_or_else(|_| "<failed to render snippet>".to_string());
    format!(
        "Skill installed. Merge the following block into \
         ~/.gemini/settings.json (create the file if it does not exist), \
         then restart Gemini CLI:\n\
         \n\
         {snippet_text}"
    )
}

fn goose_hint(_data_dir: Option<&Path>) -> String {
    // Goose runs recipes by filename stem; `aimx.yaml` → `goose run --recipe aimx`.
    //
    // The team-sharing blurb is intentionally static (no `std::env::var`
    // lookup) so `aimx agent-setup --list` is deterministic across
    // developer shells. Reading GOOSE_RECIPE_GITHUB_REPO at hint-render
    // time made snapshot-style tests of `--list` flake when the env var
    // happened to be set locally; instead, reference the variable by name
    // so the user knows the mechanism without the output depending on
    // their current environment.
    "Recipe installed. Run it with:\n\
     \n\
     \x20\x20goose run --recipe aimx\n\
     \n\
     To share the recipe with your team, commit \
     ~/.config/goose/recipes/aimx.yaml into the GitHub repo referenced by \
     $GOOSE_RECIPE_GITHUB_REPO; Goose loads recipes from that repo when \
     the variable is set.\n"
        .to_string()
}

fn hermes_hint(data_dir: Option<&Path>) -> String {
    // Hermes Agent does NOT expose a shell-side CLI for registering external
    // MCP servers. `hermes mcp serve` runs Hermes itself as an MCP server
    // (the opposite direction); the canonical registration path per the
    // official docs is editing ~/.hermes/config.yaml directly. We print a
    // YAML snippet to paste under the top-level `mcp_servers:` key, then
    // the user runs `/reload-mcp` inside Hermes to pick up the new server
    // without restarting.
    //
    // The snippet is hand-rendered as YAML rather than serialized via a YAML
    // library so we avoid a serde_yaml dependency (FR-49 principle: never
    // mutate agent config files; print snippets instead). The args list uses
    // YAML inline-flow syntax so the rendered block stays compact.
    //
    // YAML flow sequences treat `,`, `[`, `]`, and `#` as structural, so any
    // `--data-dir` path containing those characters must be quoted. We route
    // the path through `serde_json::to_string` to produce a valid YAML
    // double-quoted scalar (matches how `rewrite_recipe_data_dir` handles the
    // same problem for Goose recipes).
    let args_inline = match data_dir {
        Some(dd) => {
            let dd_str = dd.to_string_lossy();
            let quoted =
                serde_json::to_string(dd_str.as_ref()).unwrap_or_else(|_| format!("\"{dd_str}\""));
            format!("[--data-dir, {quoted}, mcp]")
        }
        None => "[mcp]".to_string(),
    };
    format!(
        "Skill installed at ~/.hermes/skills/aimx/. Add the following block \
         to the top-level `mcp_servers:` key in ~/.hermes/config.yaml \
         (create the key if it does not yet exist):\n\
         \n\
         \x20\x20mcp_servers:\n\
         \x20\x20\x20\x20aimx:\n\
         \x20\x20\x20\x20\x20\x20command: /usr/local/bin/aimx\n\
         \x20\x20\x20\x20\x20\x20args: {args_inline}\n\
         \x20\x20\x20\x20\x20\x20enabled: true\n\
         \n\
         Then run `/reload-mcp` inside Hermes to pick up the new server \
         without restarting. (Hermes loads MCP servers from \
         ~/.hermes/config.yaml; `/reload-mcp` re-reads that file at runtime.)"
    )
}

fn openclaw_hint(data_dir: Option<&Path>) -> String {
    // OpenClaw exposes `openclaw mcp set <name> <json>`. The user can wire
    // MCP with one pasted command. Route the JSON through serde_json so
    // paths with `"` or `\` escape correctly. Then POSIX-shell-escape the
    // resulting JSON so a `--data-dir` path containing `'` doesn't terminate
    // the outer single-quoted shell string prematurely.
    let args: Vec<String> = match data_dir {
        Some(dd) => vec![
            "--data-dir".to_string(),
            dd.to_string_lossy().into_owned(),
            "mcp".to_string(),
        ],
        None => vec!["mcp".to_string()],
    };
    let snippet = serde_json::json!({
        "command": "/usr/local/bin/aimx",
        "args": args,
    });
    let snippet_text = serde_json::to_string(&snippet)
        .unwrap_or_else(|_| "<failed to render snippet>".to_string());
    let shell_quoted = posix_single_quote(&snippet_text);
    format!(
        "Skill installed. Register the aimx MCP server with OpenClaw:\n\
         \n\
         \x20\x20openclaw mcp set aimx {shell_quoted}\n\
         \n\
         Restart the OpenClaw gateway after registration so the new server \
         is loaded."
    )
}

/// Wrap `s` in POSIX-style single quotes, escaping any embedded `'` via the
/// standard `'\''` concatenation trick so the result is safe to paste into
/// a shell as a single word. The input may contain any bytes except NUL.
fn posix_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            // Close the quoted run, emit an escaped quote, reopen.
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
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
    run_with_env_to_writer(agent, list, force, print, data_dir, env, &mut io::stdout())
}

/// Testable core of `run_with_env`: writes install output, `--list` output,
/// or the bare-invocation registry dump (plus usage-hint footer) to `out`
/// rather than real stdout so tests can capture and assert on it.
pub fn run_with_env_to_writer(
    agent: Option<String>,
    list: bool,
    force: bool,
    print: bool,
    data_dir: Option<&Path>,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    if list {
        print_registry_to_writer(env, out)?;
        return Ok(());
    }

    // Enforce the per-user constraint before any install- or bare-invocation
    // work so every error-path (positional agent, unknown agent, bare) surfaces
    // the same root refusal instead of an install attempt or registry dump.
    if env.is_root() {
        return Err("agent-setup is a per-user operation. Run without sudo or as root".into());
    }

    let opts = InstallOptions {
        force,
        print,
        data_dir,
    };

    let spec = match agent {
        Some(name) => find_agent(&name).ok_or_else(|| {
            format!("unknown agent '{name}'; run `aimx agent-setup --list` to see supported agents")
        })?,
        None => {
            // Bare invocation: print the registry plus a usage-hint footer
            // and exit cleanly. Same output on TTY and non-TTY; no prompt.
            print_registry_to_writer(env, out)?;
            writeln!(
                out,
                "Run `aimx agent-setup <agent>` to install one of the agents \
                 above, or `aimx agent-setup --list` to print this list again."
            )?;
            return Ok(());
        }
    };

    install_to_writer(spec, &opts, env, out)
}

fn print_registry_to_writer(env: &dyn AgentEnv, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}", term::header("Supported agents:"))?;
    writeln!(out)?;
    for spec in registry() {
        let dest = resolve_dest(spec.dest_template, env)
            .unwrap_or_else(|_| PathBuf::from(spec.dest_template));
        writeln!(out, "  {}", term::highlight(spec.name))?;
        writeln!(out, "    destination: {}", dest.display())?;
        writeln!(out, "    install:     aimx agent-setup {}", spec.name)?;
        let hint = (spec.activation_hint)(None);
        let mut hint_lines = hint.lines();
        if let Some(first) = hint_lines.next() {
            writeln!(out, "    activation:  {first}")?;
            for line in hint_lines {
                writeln!(out, "                 {line}")?;
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Writes user-facing output to `out`. Called from `run_with_env_to_writer`
/// once an `AgentSpec` has been resolved from the positional `<agent>`
/// argument.
///
/// Handles `--print` (dry run; emits file list + activation hint) and the
/// normal install path (lays files down under `dest_template`, then prints
/// the activation hint).
fn install_to_writer(
    spec: &AgentSpec,
    opts: &InstallOptions,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = AGENTS_DIR.get_dir(spec.source_subdir).ok_or_else(|| {
        format!(
            "internal error: missing embedded source for '{}'",
            spec.name
        )
    })?;

    let files =
        assemble_plugin_files_with_disclosure(source, opts.data_dir, spec.progressive_disclosure)?;
    let hint = (spec.activation_hint)(opts.data_dir);

    if opts.print {
        for (rel, bytes) in &files {
            writeln!(out, "=== {} ===", rel.display())?;
            match std::str::from_utf8(bytes) {
                Ok(text) => writeln!(out, "{text}")?,
                Err(_) => writeln!(out, "<{} bytes of binary content>", bytes.len())?,
            }
        }
        // `--print` also emits the activation hint so snippet-style agents
        // (opencode, gemini) expose their MCP JSON block under dry-run.
        writeln!(out, "=== activation ===")?;
        writeln!(out, "{hint}")?;
        return Ok(());
    }

    let dest_root = resolve_dest(spec.dest_template, env)?;
    write_files(&dest_root, &files, opts.force, env)?;

    writeln!(
        out,
        "{} {}",
        term::success("Installed"),
        term::highlight(&dest_root.to_string_lossy())
    )?;
    writeln!(out, "{hint}")?;

    Ok(())
}

/// Walk the embedded plugin source, transform known files (skill header +
/// primer + optional footer, plugin.json), and return the full set of files
/// to write.
///
/// When `progressive_disclosure` is `true`, the `references/*.md` files from
/// `agents/common/references/` are included alongside the assembled SKILL.md.
/// When `false`, only the main primer is included.
///
/// Returns `(relative_path, bytes)` pairs. Relative paths are relative to
/// the install destination root.
#[cfg(test)]
pub fn assemble_plugin_files(
    source: &Dir<'_>,
    data_dir: Option<&Path>,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    assemble_plugin_files_with_disclosure(source, data_dir, false)
}

pub fn assemble_plugin_files_with_disclosure(
    source: &Dir<'_>,
    data_dir: Option<&Path>,
    progressive_disclosure: bool,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    collect_entries(source, Path::new(""), &mut out)?;

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

            // Append optional footer if present (e.g. SKILL.md.footer).
            let footer_name = "SKILL.md.footer";
            let footer_path = source
                .path()
                .parent()
                .map(|_| {
                    // Try to find footer relative to where the header lives
                    let header_dir = rel.parent().unwrap_or(Path::new(""));
                    header_dir.join(footer_name)
                })
                .unwrap_or_else(|| PathBuf::from(footer_name));

            // Look for the footer in the source directory entries we collected
            // (they were drained, so check the embedded source directly).
            if let Some(footer_file) = find_file_in_dir(source, &footer_path) {
                combined.extend_from_slice(footer_file);
            }

            transformed.push((target, combined));
            continue;
        }

        // Goose recipe: `<name>.yaml.header` + indented primer → `<name>.yaml`.
        // The primer is appended as a YAML block scalar (each line prefixed by
        // two spaces) so it sits under a `prompt: |` key in the header.
        if rel
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".yaml.header"))
        {
            let stem = rel.file_name().unwrap().to_string_lossy();
            let new_name = stem.trim_end_matches(".header").to_string();
            let target = rel.with_file_name(new_name);

            let mut combined = bytes.clone();
            let primer_text = std::str::from_utf8(primer)
                .map_err(|e| format!("common primer not valid UTF-8: {e}"))?;
            let indented = indent_block(primer_text, "  ");
            combined.extend_from_slice(indented.as_bytes());

            let final_bytes = if let Some(dd) = data_dir {
                let text = std::str::from_utf8(&combined)
                    .map_err(|e| format!("recipe yaml not valid UTF-8: {e}"))?;
                rewrite_recipe_data_dir(text, dd)?.into_bytes()
            } else {
                combined
            };

            transformed.push((target, final_bytes));
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

        // Skip README.md at the top of the plugin source; it is developer-facing,
        // not an artifact to install.
        //
        // NOTE: this match is deliberately top-level only (`rel.as_os_str()`
        // rather than `rel.file_name()`), so a nested file such as
        // `docs/README.md` inside a plugin tree would still be installed.
        // Keep this scoping if you touch the filter.
        if rel.as_os_str() == "README.md" {
            continue;
        }

        // Skip .footer files; they are consumed during header+primer assembly
        // and should not appear as standalone files in the output.
        if rel
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".footer"))
        {
            continue;
        }

        transformed.push((rel, bytes));
    }

    if progressive_disclosure && let Some(refs_dir) = AGENTS_DIR.get_dir("common/references") {
        // Place references/ as a sibling of the assembled SKILL.md.
        let skill_parent = transformed
            .iter()
            .find(|(rel, _)| rel.file_name().is_some_and(|n| n == "SKILL.md"))
            .map(|(rel, _)| rel.parent().unwrap_or(Path::new("")).to_path_buf())
            .unwrap_or_default();

        for entry in refs_dir.entries() {
            if let DirEntry::File(f) = entry {
                let name = f
                    .path()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let rel = skill_parent.join("references").join(&name);
                transformed.push((rel, f.contents().to_vec()));
            }
        }
    }

    transformed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(transformed)
}

fn find_file_in_dir<'a>(dir: &'a Dir<'_>, rel_path: &Path) -> Option<&'a [u8]> {
    let full_path = dir.path().join(rel_path);
    dir.get_file(&full_path).map(|f| f.contents())
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
/// file. Acceptable because `plugin.json` has no comments or meaningful
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

/// Prefix every line of `text` with `prefix`. Empty lines stay empty (no
/// trailing whitespace) to keep YAML block scalars tidy.
fn indent_block(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.lines().count() * prefix.len());
    for line in text.split_inclusive('\n') {
        // Split line body from its trailing newline (if any) so blank lines
        // are emitted as just "\n", not "  \n".
        let (body, newline) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        if body.is_empty() {
            out.push_str(newline);
        } else {
            out.push_str(prefix);
            out.push_str(body);
            out.push_str(newline);
        }
    }
    out
}

/// Rewrite the `args:` array on the first stdio extension in a Goose recipe
/// YAML so the command runs with `--data-dir <path>`. Implemented as a
/// simple line-oriented transform. We inject `--data-dir` + path entries
/// before the existing `- mcp` line under `args:`. This avoids pulling in a
/// YAML serializer that would rewrite the whole file and risk breaking the
/// `prompt: |` block scalar.
///
/// Returns `Err` if the expected injection point (an indented `- ` list item
/// under `args:`) is not found. This keeps misuse loud if the recipe header
/// is ever restructured (e.g. to `args: []` inline) so `--data-dir` is not
/// silently dropped.
pub fn rewrite_recipe_data_dir(yaml_text: &str, data_dir: &Path) -> Result<String, String> {
    let dd = data_dir.to_string_lossy().into_owned();
    let mut out = String::with_capacity(yaml_text.len() + 64);
    let mut in_args = false;
    let mut injected = false;

    for line in yaml_text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if !injected && !in_args && trimmed.trim_start() == "args:" {
            in_args = true;
            out.push_str(line);
            continue;
        }

        if in_args && !injected {
            // Expect `      - mcp` (indented list item). When we see the
            // first list item, inject --data-dir entries before it.
            if let Some(idx) = trimmed.find("- ") {
                let indent = &trimmed[..idx];
                out.push_str(indent);
                out.push_str("- --data-dir\n");
                out.push_str(indent);
                out.push_str("- ");
                // Quote the path for YAML. A double-quoted scalar escapes
                // special chars safely via serde_json's string serializer.
                let quoted = serde_json::to_string(&dd).unwrap_or_else(|_| format!("\"{dd}\""));
                out.push_str(&quoted);
                out.push('\n');
                injected = true;
                // Fall through to emit the original `- mcp` line.
            } else if !trimmed.trim_start().is_empty() && !trimmed.starts_with(' ') {
                // Left the args block before seeing a list item. Abort injection.
                in_args = false;
            }
        }

        out.push_str(line);
    }

    if !injected {
        return Err(
            "rewrite_recipe_data_dir: could not find an `args:` block with a `- ` list \
             item to inject `--data-dir` before; recipe header may have been restructured"
                .to_string(),
        );
    }

    Ok(out)
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
        assert!(
            plugin.contains("\"mcpServers\""),
            "claude-code plugin.json must declare `mcpServers`: {plugin}"
        );
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
        assert!(plugin.contains("\"mcpServers\""));
    }

    #[test]
    fn list_mode_runs_without_agent_name() {
        // `--list` prints the registry without the trailing
        // `Run \`aimx agent-setup <agent>\`` usage-hint footer. The footer is
        // reserved for bare invocation; this lock-in prevents the two
        // output shapes from converging.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut out: Vec<u8> = Vec::new();
        run_with_env_to_writer(None, true, false, false, None, &env, &mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert!(
            !rendered.contains("Run `aimx agent-setup <agent>`"),
            "--list output must not include the bare-invocation usage hint: {rendered}"
        );
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
    fn registry_contains_codex_opencode_gemini_agents() {
        for name in ["codex", "opencode", "gemini"] {
            assert!(find_agent(name).is_some(), "registry missing agent: {name}");
        }
    }

    #[test]
    fn install_codex_lays_out_expected_files() {
        // Codex CLI does not auto-discover plugins under
        // `~/.codex/plugins/`. Its MCP wiring lives in `~/.codex/config.toml`,
        // managed via `codex mcp add`. The installer therefore ships a flat
        // skill at `~/.codex/skills/aimx/SKILL.md` (same shape as Gemini and
        // OpenCode) and emits a `codex mcp add` command in the activation
        // hint for the user to run once. No plugin manifest is written.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("codex".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".codex/skills/aimx");
        assert!(
            dest.join("SKILL.md").exists(),
            "skill must be installed as a flat `SKILL.md` under ~/.codex/skills/aimx/"
        );
        assert!(!dest.join(".codex-plugin").exists(), "no plugin manifest");
        assert!(!dest.join("plugin.json").exists());
        assert!(!dest.join("README.md").exists());
        assert!(!dest.join("SKILL.md.header").exists());
        assert!(
            !tmp.path().join(".codex/plugins").exists(),
            "no plugin dir is written; Codex CLI does not scan ~/.codex/plugins/"
        );

        let skill = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(skill.starts_with("---\n"));
        assert!(skill.contains("name: aimx"));
        assert!(skill.contains("MCP tools"));
        assert!(skill.contains("mailbox_create"));
    }

    #[test]
    fn codex_activation_hint_includes_codex_mcp_add_command() {
        // Regression guard: the hint must instruct the user to run the
        // canonical Codex MCP registration command, not just say
        // "restart Codex".
        let spec = find_agent("codex").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("codex mcp add aimx"));
        assert!(hint.contains("/usr/local/bin/aimx"));
        assert!(hint.contains(" mcp"));
    }

    #[test]
    fn codex_activation_hint_with_data_dir_includes_flag() {
        let spec = find_agent("codex").unwrap();
        let custom = PathBuf::from("/custom/aimx-data");
        let hint = (spec.activation_hint)(Some(&custom));
        assert!(hint.contains("--data-dir"));
        assert!(hint.contains("/custom/aimx-data"));
    }

    #[test]
    fn codex_activation_hint_shell_escapes_single_quote_in_data_dir() {
        let spec = find_agent("codex").unwrap();
        let quoted = PathBuf::from("/tmp/o'hare/aimx");
        let hint = (spec.activation_hint)(Some(&quoted));
        // `posix_single_quote` expands `'` into `'\''` so the resulting
        // shell token is safe to paste.
        assert!(
            hint.contains(r"'/tmp/o'\''hare/aimx'"),
            "expected shell-escaped path, got hint:\n{hint}"
        );
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
    fn claude_code_activation_hint_instructs_claude_mcp_add() {
        // S44-3: Claude Code does NOT auto-activate MCP servers from
        // plugins/installed_plugins.json (confirmed with claude -p against
        // aimx plugin on the 2026-04-17 test VPS). The hint must instruct
        // the operator to run `claude mcp add --scope user aimx ...`, mirroring
        // Codex's hint shape.
        let spec = find_agent("claude-code").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(
            hint.contains("claude mcp add --scope user aimx"),
            "hint must instruct `claude mcp add`: {hint}"
        );
        assert!(
            hint.contains("/usr/local/bin/aimx"),
            "hint must include the aimx binary path: {hint}"
        );
        assert!(hint.contains("Restart Claude Code"), "got: {hint}");
        // No `--data-dir` argument when the default data dir is used.
        assert!(!hint.contains("--data-dir"), "got: {hint}");
    }

    #[test]
    fn claude_code_activation_hint_embeds_data_dir_override() {
        let spec = find_agent("claude-code").unwrap();
        let hint = (spec.activation_hint)(Some(Path::new("/custom/data")));
        assert!(
            hint.contains("--data-dir '/custom/data'"),
            "hint must splice --data-dir with POSIX single-quote escaping: {hint}"
        );
        assert!(
            hint.contains("claude mcp add --scope user aimx"),
            "got: {hint}"
        );
    }

    #[test]
    fn codex_activation_hint_mentions_codex() {
        let spec = find_agent("codex").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("Codex"));
        assert!(
            hint.contains("~/.codex/config.toml"),
            "hint should name the actual MCP-config file so users aren't misled into looking at ~/.codex/plugins/"
        );
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
        // Capture via install_to_writer so we can assert on the actual
        // printed bytes, not just that no files were written.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        let spec = find_agent("opencode").unwrap();
        let opts = InstallOptions {
            force: false,
            print: true,
            data_dir: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();

        assert!(
            printed.contains("=== activation ==="),
            "printed output missing activation marker: {printed}"
        );
        assert!(
            printed.contains("\"mcp\""),
            "printed activation snippet missing `mcp` key: {printed}"
        );
        assert!(
            printed.contains("\"aimx\""),
            "printed activation snippet missing `aimx` key: {printed}"
        );
        assert!(
            printed.contains("/usr/local/bin/aimx"),
            "printed activation snippet missing aimx path: {printed}"
        );
        assert!(
            !tmp.path().join(".config/opencode").exists(),
            "--print must not write files"
        );

        // Gemini --print also emits its mcpServers snippet.
        let spec = find_agent("gemini").unwrap();
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();
        assert!(printed.contains("=== activation ==="));
        assert!(
            printed.contains("\"mcpServers\""),
            "gemini activation snippet missing `mcpServers`: {printed}"
        );
        assert!(printed.contains("/usr/local/bin/aimx"));
        assert!(!tmp.path().join(".gemini").exists());
    }

    #[test]
    fn print_snippet_escapes_data_dir_with_special_chars() {
        // A data-dir path containing a double-quote or backslash must not
        // produce a broken JSON snippet; serde_json escapes it for us.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        let spec = find_agent("opencode").unwrap();
        let weird = PathBuf::from("/tmp/has\"quote\\and-backslash");
        let opts = InstallOptions {
            force: false,
            print: true,
            data_dir: Some(&weird),
        };
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();

        // Extract the activation section and confirm it parses as JSON.
        let (_, after) = printed.split_once("=== activation ===\n").unwrap();
        let snippet = after
            .lines()
            .skip_while(|l| l.trim().is_empty() || l.starts_with("Skill installed"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: serde_json::Value = serde_json::from_str(snippet.trim())
            .unwrap_or_else(|e| panic!("activation snippet not valid JSON: {e}\n{snippet}"));
        let cmd = parsed
            .pointer("/mcp/aimx/command")
            .and_then(|v| v.as_array())
            .expect("command array missing");
        let last_path = cmd.get(2).and_then(|v| v.as_str()).unwrap();
        assert_eq!(last_path, "/tmp/has\"quote\\and-backslash");
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
        // Codex ships as a flat skill (no plugin manifest), so the
        // assembled file is at the package root as `SKILL.md`, not nested
        // under `skills/aimx/`.
        let source = AGENTS_DIR.get_dir("codex").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "SKILL.md")
            .expect("assembled SKILL.md should be present at the source root");

        let header = AGENTS_DIR
            .get_file("codex/SKILL.md.header")
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
    fn registry_lists_seven_agents_in_canonical_order() {
        let names: Vec<&str> = registry().iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "claude-code",
                "codex",
                "opencode",
                "gemini",
                "goose",
                "openclaw",
                "hermes",
            ]
        );
    }

    #[test]
    fn registry_contains_goose_openclaw_agents() {
        for name in ["goose", "openclaw"] {
            assert!(find_agent(name).is_some(), "registry missing agent: {name}");
        }
    }

    #[test]
    fn install_goose_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("goose".into()), false, false, false, None, &env).unwrap();

        // Default XDG_CONFIG_HOME = $HOME/.config, so recipe lives at
        // $HOME/.config/goose/recipes/aimx.yaml.
        let recipe = tmp.path().join(".config/goose/recipes/aimx.yaml");
        assert!(recipe.exists(), "expected recipe at {}", recipe.display());
        // .header file must be absent; it is a source template, not an artifact.
        assert!(
            !tmp.path()
                .join(".config/goose/recipes/aimx.yaml.header")
                .exists()
        );
        // README.md is developer-facing and must not be installed.
        assert!(!tmp.path().join(".config/goose/recipes/README.md").exists());

        let text = std::fs::read_to_string(&recipe).unwrap();
        assert!(text.contains("title: \"aimx email\""), "recipe: {text}");
        assert!(text.contains("prompt: |"), "recipe: {text}");
        // Primer content appears indented as part of the prompt block.
        assert!(
            text.contains("  # aimx primer for agents"),
            "recipe: {text}"
        );
        assert!(
            text.contains("  - `mailbox_create(name)`"),
            "recipe: {text}"
        );
        // Extensions section references the aimx stdio MCP server.
        assert!(text.contains("type: stdio"), "recipe: {text}");
        assert!(text.contains("name: aimx"), "recipe: {text}");
        assert!(text.contains("cmd: /usr/local/bin/aimx"), "recipe: {text}");
        // Default install has no --data-dir in args.
        assert!(!text.contains("--data-dir"));
    }

    #[test]
    fn install_goose_respects_xdg_config_home_override() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        let xdg = tmp.path().join("alt-xdg");
        env.xdg = Some(xdg.clone());

        run_with_env(Some("goose".into()), false, false, false, None, &env).unwrap();

        assert!(xdg.join("goose/recipes/aimx.yaml").exists());
    }

    #[test]
    fn install_goose_with_custom_data_dir_rewrites_recipe_args() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let custom = PathBuf::from("/custom/aimx-data");

        run_with_env(
            Some("goose".into()),
            false,
            false,
            false,
            Some(&custom),
            &env,
        )
        .unwrap();

        let recipe = tmp.path().join(".config/goose/recipes/aimx.yaml");
        let text = std::fs::read_to_string(&recipe).unwrap();
        assert!(text.contains("--data-dir"), "recipe: {text}");
        assert!(text.contains("/custom/aimx-data"), "recipe: {text}");
        // The original `- mcp` entry must still be present after injection.
        assert!(text.contains("- mcp"), "recipe: {text}");
    }

    #[test]
    fn goose_activation_hint_mentions_recipe_command() {
        let spec = find_agent("goose").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("goose run --recipe aimx"));
    }

    #[test]
    fn goose_activation_hint_mentions_github_repo_variable() {
        // The hint must reference GOOSE_RECIPE_GITHUB_REPO by name so users
        // discover the team-sharing mechanism. The output is now
        // deterministic. It does not depend on whether the variable is
        // set in the caller's shell (so `aimx agent-setup --list` is stable
        // across developer environments).
        let spec = find_agent("goose").unwrap();

        // Set it to one value: hint must not interpolate it.
        // SAFETY: these calls modify process environment; test is isolated
        // by not asserting on value-dependent output.
        unsafe {
            std::env::set_var("GOOSE_RECIPE_GITHUB_REPO", "myorg/goose-recipes");
        }
        let hint_with = (spec.activation_hint)(None);
        unsafe {
            std::env::remove_var("GOOSE_RECIPE_GITHUB_REPO");
        }
        let hint_without = (spec.activation_hint)(None);

        assert_eq!(
            hint_with, hint_without,
            "goose hint must be deterministic regardless of GOOSE_RECIPE_GITHUB_REPO"
        );
        assert!(hint_without.contains("GOOSE_RECIPE_GITHUB_REPO"));
        assert!(hint_without.contains("aimx.yaml"));
        // Must not leak a concrete repo slug; we only reference the var name.
        assert!(!hint_without.contains("myorg/goose-recipes"));
    }

    #[test]
    fn install_openclaw_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("openclaw".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".openclaw/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(!dest.join("SKILL.md.header").exists());
        assert!(!dest.join("README.md").exists());

        let skill = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(
            skill.starts_with("---\n"),
            "missing YAML frontmatter: {skill:.200}"
        );
        assert!(skill.contains("name: aimx"));
        assert!(skill.contains("description:"));
        assert!(skill.contains("mailbox_create"));
        assert!(skill.contains("Trust model"));
    }

    #[test]
    fn openclaw_activation_hint_prints_mcp_set_command() {
        let spec = find_agent("openclaw").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("openclaw mcp set aimx"));
        assert!(hint.contains("/usr/local/bin/aimx"));
        assert!(hint.contains("\"args\""));
        assert!(hint.contains("mcp"));
        assert!(!hint.contains("--data-dir"));

        let hint_custom = (spec.activation_hint)(Some(Path::new("/custom/aimx-data")));
        assert!(hint_custom.contains("--data-dir"));
        assert!(hint_custom.contains("/custom/aimx-data"));
    }

    /// Minimal POSIX single-quoted shell-word unquoter for tests. Accepts a
    /// string that starts and ends with `'` and may contain `'\''` escape
    /// sequences (close-quote, escaped literal quote, reopen-quote). Any
    /// other escape or multiple unquoted-word concatenation fails with
    /// `None`. This is not a general-purpose shell parser; it is scoped
    /// to validating our own `posix_single_quote` output.
    fn shell_unquote_single(s: &str) -> Option<String> {
        let bytes = s.as_bytes();
        if bytes.len() < 2 || bytes[0] != b'\'' || bytes[bytes.len() - 1] != b'\'' {
            return None;
        }
        let mut out = String::with_capacity(s.len());
        let mut i = 1;
        let end = bytes.len() - 1;
        while i < end {
            if bytes[i] == b'\'' {
                // Must be `'\''`: close, literal-quote, reopen.
                if i + 3 >= bytes.len() || &bytes[i..i + 4] != b"'\\''" {
                    return None;
                }
                out.push('\'');
                i += 4;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        Some(out)
    }

    #[test]
    fn openclaw_activation_hint_escapes_single_quote_in_data_dir() {
        // A `--data-dir` path containing `'` must not terminate the outer
        // single-quoted shell string prematurely. We POSIX-escape the JSON
        // body with the `'\''` concatenation trick before wrapping it in
        // single quotes. Verify the produced shell-quoted word unquotes
        // back to valid JSON that carries the original path byte-for-byte.
        let spec = find_agent("openclaw").unwrap();
        let weird = PathBuf::from("/home/user's data/aimx");
        let hint = (spec.activation_hint)(Some(&weird));

        let line = hint
            .lines()
            .find(|l| l.trim_start().starts_with("openclaw mcp set aimx "))
            .expect("expected an `openclaw mcp set aimx` command line");
        // Strip the leading `openclaw mcp set aimx ` prefix to isolate the
        // quoted JSON argument.
        let quoted_arg = line
            .trim_start()
            .strip_prefix("openclaw mcp set aimx ")
            .unwrap();
        let json_body = shell_unquote_single(quoted_arg).unwrap_or_else(|| {
            panic!("shell-quoted arg did not parse as a single POSIX-quoted word: {quoted_arg}")
        });

        let parsed: serde_json::Value = serde_json::from_str(&json_body)
            .unwrap_or_else(|e| panic!("shell-unquoted JSON not valid: {e}\n{json_body}"));
        let args = parsed.get("args").and_then(|v| v.as_array()).unwrap();
        let path = args.get(1).and_then(|v| v.as_str()).unwrap();
        assert_eq!(path, "/home/user's data/aimx");
    }

    #[test]
    fn posix_single_quote_escapes_embedded_quote() {
        // Standard POSIX trick: close, emit an escaped literal `'`, reopen.
        assert_eq!(posix_single_quote("a'b"), "'a'\\''b'");
        // No special chars: plain wrap.
        assert_eq!(posix_single_quote("abc"), "'abc'");
        // Empty string: still produces an empty quoted pair.
        assert_eq!(posix_single_quote(""), "''");
    }

    #[test]
    fn openclaw_activation_hint_escapes_special_chars_in_data_dir() {
        // Paths with special chars must serialize via serde_json so the
        // printed command stays a valid shell-quoted JSON argument.
        let spec = find_agent("openclaw").unwrap();
        let weird = PathBuf::from("/tmp/has\"quote\\and-backslash");
        let hint = (spec.activation_hint)(Some(&weird));

        // Extract the JSON body between the first pair of single quotes.
        let start = hint.find('\'').unwrap() + 1;
        let end = hint.rfind('\'').unwrap();
        let json_body = &hint[start..end];

        let parsed: serde_json::Value = serde_json::from_str(json_body)
            .unwrap_or_else(|e| panic!("activation snippet not valid JSON: {e}\n{json_body}"));
        let args = parsed.get("args").and_then(|v| v.as_array()).unwrap();
        // args[1] is the --data-dir path (args = ["--data-dir", <path>, "mcp"]).
        let path = args.get(1).and_then(|v| v.as_str()).unwrap();
        assert_eq!(path, "/tmp/has\"quote\\and-backslash");
    }

    #[test]
    fn assembled_goose_recipe_contains_indented_primer() {
        let source = AGENTS_DIR.get_dir("goose").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, recipe_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "aimx.yaml")
            .expect("assembled aimx.yaml should be present");

        let header = AGENTS_DIR
            .get_file("goose/aimx.yaml.header")
            .unwrap()
            .contents();
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();

        // The recipe should be header + indent_block(primer, "  ").
        let mut expected = Vec::new();
        expected.extend_from_slice(header);
        expected
            .extend_from_slice(indent_block(std::str::from_utf8(primer).unwrap(), "  ").as_bytes());

        assert_eq!(recipe_bytes, &expected);
    }

    #[test]
    fn assembled_openclaw_skill_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("openclaw").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("openclaw/SKILL.md.header")
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
    fn indent_block_preserves_blank_lines_without_trailing_whitespace() {
        let input = "first\n\nthird\n";
        let got = indent_block(input, "  ");
        assert_eq!(got, "  first\n\n  third\n");
    }

    #[test]
    fn indent_block_handles_multiline_without_trailing_newline() {
        // When the final line is unterminated, the prefix must still be
        // applied and the absence of the trailing newline preserved, so
        // callers that append this block to a larger document don't get a
        // spurious blank line.
        let input = "first\nsecond";
        let got = indent_block(input, "  ");
        assert_eq!(got, "  first\n  second");
    }

    #[test]
    fn rewrite_recipe_data_dir_injects_args_before_mcp() {
        let input = "extensions:\n  - type: stdio\n    name: aimx\n    cmd: /usr/local/bin/aimx\n    args:\n      - mcp\nprompt: |\n  body\n";
        let got = rewrite_recipe_data_dir(input, Path::new("/custom/path")).unwrap();
        assert!(got.contains("- --data-dir"), "got: {got}");
        assert!(got.contains("\"/custom/path\""), "got: {got}");
        assert!(got.contains("- mcp"), "got: {got}");
        // Order matters: --data-dir must appear before mcp.
        let dd_idx = got.find("- --data-dir").unwrap();
        let mcp_idx = got.find("- mcp").unwrap();
        assert!(dd_idx < mcp_idx, "data-dir should come before mcp");
    }

    #[test]
    fn rewrite_recipe_data_dir_errors_when_args_block_has_no_list_item() {
        // `args:` is followed by a sibling key at the same indent level,
        // i.e. the block is effectively empty. Injection point is missing,
        // so the function must surface an error rather than silently
        // dropping the `--data-dir` flag.
        let input = "extensions:\n  - type: stdio\n    name: aimx\n    args:\nprompt: |\n  body\n";
        let err = rewrite_recipe_data_dir(input, Path::new("/custom/path")).unwrap_err();
        assert!(err.contains("could not find"), "unexpected error: {err}");
    }

    #[test]
    fn rewrite_recipe_data_dir_errors_when_no_args_block_present() {
        // No `args:` key anywhere; function cannot inject and must error.
        let input = "extensions:\n  - type: stdio\n    name: aimx\nprompt: |\n  body\n";
        let err = rewrite_recipe_data_dir(input, Path::new("/custom/path")).unwrap_err();
        assert!(err.contains("could not find"), "unexpected error: {err}");
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

    #[test]
    fn primer_line_count_within_target_range() {
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .expect("common/aimx-primer.md must exist")
            .contents();
        let text = std::str::from_utf8(primer).expect("primer must be valid UTF-8");
        let line_count = text.lines().count();
        // Target: 300–500 lines (soft cap).
        assert!(
            (300..=500).contains(&line_count),
            "main primer has {line_count} lines; target range is 300–500"
        );
    }

    #[test]
    fn references_directory_exists_in_embedded_assets() {
        let refs_dir = AGENTS_DIR.get_dir("common/references");
        assert!(
            refs_dir.is_some(),
            "agents/common/references/ must exist in embedded assets"
        );
        let refs_dir = refs_dir.unwrap();
        let filenames: Vec<String> = refs_dir
            .entries()
            .iter()
            .filter_map(|e| match e {
                DirEntry::File(f) => f
                    .path()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        assert!(filenames.contains(&"mcp-tools.md".to_string()));
        assert!(filenames.contains(&"frontmatter.md".to_string()));
        assert!(filenames.contains(&"workflows.md".to_string()));
        assert!(filenames.contains(&"troubleshooting.md".to_string()));
    }

    #[test]
    fn progressive_disclosure_agent_gets_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        // Claude Code has progressive_disclosure: true
        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".claude/plugins/aimx");
        let refs = dest.join("skills/aimx/references");
        assert!(
            refs.join("mcp-tools.md").exists(),
            "progressive-disclosure agent should have references/mcp-tools.md"
        );
        assert!(refs.join("frontmatter.md").exists());
        assert!(refs.join("workflows.md").exists());
        assert!(refs.join("troubleshooting.md").exists());

        // Verify references content is non-empty and matches embedded assets
        let installed = std::fs::read_to_string(refs.join("mcp-tools.md")).unwrap();
        let embedded = AGENTS_DIR
            .get_file("common/references/mcp-tools.md")
            .unwrap()
            .contents();
        assert_eq!(installed.as_bytes(), embedded);
    }

    #[test]
    fn non_progressive_disclosure_agent_has_no_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        // OpenCode has progressive_disclosure: false
        run_with_env(Some("opencode".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".config/opencode/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(
            !dest.join("references").exists(),
            "non-progressive-disclosure agent should NOT have references/"
        );
    }

    #[test]
    fn codex_progressive_disclosure_installs_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("codex".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".codex/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("references/mcp-tools.md").exists());
        assert!(dest.join("references/frontmatter.md").exists());
        assert!(dest.join("references/workflows.md").exists());
        assert!(dest.join("references/troubleshooting.md").exists());
    }

    #[test]
    fn openclaw_progressive_disclosure_installs_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("openclaw".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".openclaw/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("references/mcp-tools.md").exists());
        assert!(dest.join("references/frontmatter.md").exists());
    }

    #[test]
    fn gemini_no_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("gemini".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".gemini/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(!dest.join("references").exists());
    }

    #[test]
    fn goose_no_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("goose".into()), false, false, false, None, &env).unwrap();

        let recipe = tmp.path().join(".config/goose/recipes/aimx.yaml");
        assert!(recipe.exists());
        assert!(!tmp.path().join(".config/goose/recipes/references").exists());
    }

    #[test]
    fn print_mode_shows_references_for_progressive_disclosure() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        let spec = find_agent("codex").unwrap();
        let opts = InstallOptions {
            force: false,
            print: true,
            data_dir: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();

        assert!(
            printed.contains("=== references/mcp-tools.md ==="),
            "print mode should show references for progressive-disclosure agents"
        );
        assert!(printed.contains("=== references/frontmatter.md ==="));
        assert!(printed.contains("=== references/workflows.md ==="));
        assert!(printed.contains("=== references/troubleshooting.md ==="));
    }

    #[test]
    fn print_mode_omits_references_for_non_progressive_disclosure() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        let spec = find_agent("opencode").unwrap();
        let opts = InstallOptions {
            force: false,
            print: true,
            data_dir: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();

        assert!(
            !printed.contains("=== references/"),
            "print mode should NOT show references for non-progressive-disclosure agents"
        );
    }

    #[test]
    fn progressive_disclosure_assignments() {
        let reg = registry();
        let by_name = |n: &str| reg.iter().find(|s| s.name == n).unwrap();

        assert!(by_name("claude-code").progressive_disclosure);
        assert!(by_name("codex").progressive_disclosure);
        assert!(by_name("openclaw").progressive_disclosure);
        assert!(by_name("hermes").progressive_disclosure);

        assert!(!by_name("opencode").progressive_disclosure);
        assert!(!by_name("gemini").progressive_disclosure);
        assert!(!by_name("goose").progressive_disclosure);
    }

    #[test]
    fn author_metadata_in_claude_code_plugin_json() {
        let plugin_bytes = AGENTS_DIR
            .get_file("claude-code/.claude-plugin/plugin.json")
            .expect("plugin.json must exist")
            .contents();
        let text = std::str::from_utf8(plugin_bytes).unwrap();
        assert!(
            text.contains("U-Zyn Chua"),
            "plugin.json must have standardized author"
        );
        assert!(
            !text.contains("\"name\": \"AIMX\""),
            "plugin.json must not have AIMX as author name"
        );
    }

    #[test]
    fn author_metadata_in_all_skill_headers() {
        for agent in [
            "claude-code",
            "codex",
            "opencode",
            "gemini",
            "openclaw",
            "hermes",
        ] {
            let header_path = match agent {
                "claude-code" => "claude-code/skills/aimx/SKILL.md.header",
                _ => &format!("{agent}/SKILL.md.header"),
            };
            let header = AGENTS_DIR
                .get_file(header_path)
                .unwrap_or_else(|| panic!("missing header for {agent}"))
                .contents();
            let text = std::str::from_utf8(header).unwrap();
            assert!(
                text.contains("author: U-Zyn Chua <chua@uzyn.com>"),
                "{agent} SKILL.md.header must contain author metadata"
            );
        }
    }

    #[test]
    fn goose_header_notes_author_gap() {
        let header = AGENTS_DIR
            .get_file("goose/aimx.yaml.header")
            .expect("goose header must exist")
            .contents();
        let text = std::str::from_utf8(header).unwrap();
        assert!(
            text.contains("U-Zyn Chua"),
            "goose header must reference the author"
        );
        assert!(
            text.contains("does not support an author field"),
            "goose header must note the schema gap"
        );
    }

    #[test]
    fn no_aimx_author_placeholder_in_agents() {
        fn check_dir(dir: &Dir<'_>, path_prefix: &str) {
            for entry in dir.entries() {
                match entry {
                    DirEntry::File(f) => {
                        if let Ok(text) = std::str::from_utf8(f.contents()) {
                            let full_path = format!("{}/{}", path_prefix, f.path().display());
                            assert!(
                                !text.contains("\"author\": \"AIMX\"")
                                    && !text.contains("\"name\": \"AIMX\""),
                                "found placeholder author in {full_path}"
                            );
                            assert!(
                                !text.contains("chua@example.com"),
                                "found placeholder email in {full_path}"
                            );
                        }
                    }
                    DirEntry::Dir(sub) => {
                        check_dir(sub, &format!("{}/{}", path_prefix, sub.path().display()));
                    }
                }
            }
        }
        check_dir(&AGENTS_DIR, "agents");
    }

    #[test]
    fn primer_documents_storage_layout_plainly() {
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();
        let text = std::str::from_utf8(primer).unwrap();
        assert!(
            text.contains("/var/lib/aimx/"),
            "primer must document the datadir layout plainly (FR-50c)"
        );
        assert!(text.contains("inbox/"));
        assert!(text.contains("sent/"));
        assert!(text.contains("FR-50c"));
    }

    #[test]
    fn primer_links_references_and_runtime_readme() {
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();
        let text = std::str::from_utf8(primer).unwrap();
        assert!(text.contains("references/mcp-tools.md"));
        assert!(text.contains("references/frontmatter.md"));
        assert!(text.contains("references/workflows.md"));
        assert!(text.contains("references/troubleshooting.md"));
        assert!(text.contains("/var/lib/aimx/README.md"));
    }

    #[test]
    fn trusted_field_documented_in_primer_and_reference() {
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();
        let primer_text = std::str::from_utf8(primer).unwrap();
        assert!(primer_text.contains("`trusted`"));
        assert!(primer_text.contains("\"none\""));
        assert!(primer_text.contains("\"true\""));
        assert!(primer_text.contains("\"false\""));

        let frontmatter_ref = AGENTS_DIR
            .get_file("common/references/frontmatter.md")
            .unwrap()
            .contents();
        let ref_text = std::str::from_utf8(frontmatter_ref).unwrap();
        assert!(ref_text.contains("`trusted`"));
        assert!(ref_text.contains("\"none\""));
        assert!(ref_text.contains("\"true\""));
        assert!(ref_text.contains("\"false\""));
        assert!(ref_text.contains("trusted_senders"));
    }

    #[test]
    fn registry_contains_hermes() {
        let spec = find_agent("hermes").expect("registry must include hermes");
        assert_eq!(spec.dest_template, "$HOME/.hermes/skills/aimx");
        assert!(spec.progressive_disclosure);
    }

    #[test]
    fn install_hermes_lays_out_expected_files() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("hermes".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".hermes/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(!dest.join("SKILL.md.header").exists());
        assert!(!dest.join("README.md").exists());

        let skill = std::fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(
            skill.starts_with("---\n"),
            "missing YAML frontmatter: {skill:.200}"
        );
        assert!(skill.contains("name: aimx"));
        assert!(skill.contains("description:"));
        assert!(skill.contains("license: MIT"));
        assert!(skill.contains("metadata:"));
        assert!(skill.contains("hermes:"));
        assert!(skill.contains("mailbox_create"));
        assert!(skill.contains("Trust model"));
    }

    #[test]
    fn hermes_progressive_disclosure_installs_references() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("hermes".into()), false, false, false, None, &env).unwrap();

        let dest = tmp.path().join(".hermes/skills/aimx");
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("references/mcp-tools.md").exists());
        assert!(dest.join("references/frontmatter.md").exists());
        assert!(dest.join("references/workflows.md").exists());
        assert!(dest.join("references/troubleshooting.md").exists());
    }

    #[test]
    fn hermes_activation_hint_mentions_config_and_reload() {
        let spec = find_agent("hermes").unwrap();
        let hint = (spec.activation_hint)(None);
        assert!(hint.contains("~/.hermes/config.yaml"));
        assert!(hint.contains("/reload-mcp"));
        assert!(hint.contains("mcp_servers:"));
        assert!(hint.contains("aimx:"));
        assert!(hint.contains("command: /usr/local/bin/aimx"));
        assert!(hint.contains("args: [mcp]"));
        assert!(hint.contains("enabled: true"));
        // Default install must not leak --data-dir into the snippet.
        assert!(!hint.contains("--data-dir"));
    }

    #[test]
    fn hermes_activation_hint_with_custom_data_dir_rewrites_args() {
        let spec = find_agent("hermes").unwrap();
        let hint = (spec.activation_hint)(Some(Path::new("/custom/aimx-data")));
        // The path is routed through a JSON-string serializer so it is always
        // emitted as a YAML double-quoted scalar, even for "safe" inputs.
        assert!(hint.contains("args: [--data-dir, \"/custom/aimx-data\", mcp]"));
        // The other lines remain identical to the default form.
        assert!(hint.contains("command: /usr/local/bin/aimx"));
        assert!(hint.contains("enabled: true"));
    }

    #[test]
    fn hermes_activation_hint_escapes_yaml_flow_sensitive_chars_in_data_dir() {
        // YAML flow sequences treat `,`, `[`, `]`, and `#` as structural.
        // A `--data-dir` containing any of these MUST be quoted, or the
        // rendered snippet either fails to parse or silently produces the
        // wrong argv for Hermes. Regression coverage for the blocker raised
        // on PR #91.
        let spec = find_agent("hermes").unwrap();

        // Case 1: path with `[` and `]` (previously produced a YAML parse error).
        let hint = (spec.activation_hint)(Some(Path::new("/opt/aimx [staging]")));
        assert!(
            hint.contains("args: [--data-dir, \"/opt/aimx [staging]\", mcp]"),
            "bracketed path must be quoted: {hint}"
        );
        // Three argv entries separated by commas, not nine.
        let args_line = hint
            .lines()
            .find(|l| l.trim_start().starts_with("args:"))
            .expect("snippet must contain an args: line");
        assert_eq!(
            args_count_in_flow_sequence(args_line),
            3,
            "args list must contain exactly 3 entries: {args_line}"
        );

        // Case 2: path containing `,` (previously split into extra argv entries).
        let hint = (spec.activation_hint)(Some(Path::new("/path,with,commas")));
        assert!(
            hint.contains("args: [--data-dir, \"/path,with,commas\", mcp]"),
            "comma path must be quoted: {hint}"
        );
        let args_line = hint
            .lines()
            .find(|l| l.trim_start().starts_with("args:"))
            .unwrap();
        assert_eq!(
            args_count_in_flow_sequence(args_line),
            3,
            "args list must contain exactly 3 entries: {args_line}"
        );

        // Case 3: path containing `#` (previously triggered YAML comment handling).
        let hint = (spec.activation_hint)(Some(Path::new("/opt/aimx #archive")));
        assert!(
            hint.contains("args: [--data-dir, \"/opt/aimx #archive\", mcp]"),
            "hash path must be quoted: {hint}"
        );

        // Case 4: path containing `"` and `\`, must be JSON-escaped inside
        // the double-quoted scalar so the resulting YAML is still valid.
        let hint = (spec.activation_hint)(Some(Path::new(r#"/opt/a"b\c"#)));
        assert!(
            hint.contains(r#"args: [--data-dir, "/opt/a\"b\\c", mcp]"#),
            "quote/backslash path must be JSON-escaped: {hint}"
        );
    }

    // Count items in a YAML flow sequence like `args: [a, b, c]` by splitting
    // on the commas that live OUTSIDE any double-quoted scalar. This mirrors
    // what a real YAML parser does on the `args:` line and lets us verify
    // the argv survives round-trip without pulling in a YAML crate.
    fn args_count_in_flow_sequence(line: &str) -> usize {
        let lb = line.find('[').expect("expected `[` in flow sequence");
        let rb = line.rfind(']').expect("expected `]` in flow sequence");
        let inner = &line[lb + 1..rb];

        let mut count = 0usize;
        let mut has_content = false;
        let mut in_quotes = false;
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                escaped = false;
                has_content = true;
                continue;
            }
            if in_quotes {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_quotes = false,
                    _ => {}
                }
                has_content = true;
                continue;
            }
            match ch {
                '"' => {
                    in_quotes = true;
                    has_content = true;
                }
                ',' => {
                    if has_content {
                        count += 1;
                    }
                    has_content = false;
                }
                c if c.is_whitespace() => {}
                _ => has_content = true,
            }
        }
        if has_content {
            count += 1;
        }
        count
    }

    #[test]
    fn assembled_hermes_skill_is_header_plus_primer_byte_for_byte() {
        let source = AGENTS_DIR.get_dir("hermes").unwrap();
        let files = assemble_plugin_files(source, None).unwrap();

        let (_, skill_bytes) = files
            .iter()
            .find(|(rel, _)| rel.to_string_lossy() == "SKILL.md")
            .expect("assembled SKILL.md should be present");

        let header = AGENTS_DIR
            .get_file("hermes/SKILL.md.header")
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

    // --- Bare invocation tests ---

    #[test]
    fn bare_invocation_prints_registry_and_hint() {
        // `aimx agent-setup` with no agent and no --list must print the
        // registry plus a usage-hint footer and exit Ok(()). Works on both
        // TTY and non-TTY, no interactive prompt.
        let tmp = TempDir::new().unwrap();
        for tty in [false, true] {
            let mut env = MockEnv::new(tmp.path().to_path_buf());
            env.tty = tty;
            let mut out: Vec<u8> = Vec::new();
            run_with_env_to_writer(None, false, false, false, None, &env, &mut out).unwrap();
            let rendered = String::from_utf8(out).unwrap();

            for spec in registry() {
                assert!(
                    rendered.contains(spec.name),
                    "registry output missing agent {:?} (tty={tty}): {rendered}",
                    spec.name
                );
            }
            assert!(
                rendered.contains("aimx agent-setup <agent>"),
                "missing usage hint (tty={tty}): {rendered}"
            );
            assert!(
                !tmp.path().join(".claude").exists(),
                "bare invocation must not install anything (tty={tty})"
            );
        }
    }

    #[test]
    fn bare_invocation_root_is_refused() {
        // Root + no agent must still be refused up front (before any
        // registry output) so sudo mistakes get the same friendly
        // "per-user" error as the install paths.
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        env.root = true;
        env.tty = true;
        let mut out: Vec<u8> = Vec::new();
        let err =
            run_with_env_to_writer(None, false, false, false, None, &env, &mut out).unwrap_err();
        assert!(
            err.to_string().contains("per-user"),
            "unexpected error: {err}"
        );
        assert!(
            out.is_empty(),
            "no registry output expected when root is rejected up front: {}",
            String::from_utf8_lossy(&out)
        );
        assert!(!tmp.path().join(".claude").exists());
    }

    #[test]
    fn bare_invocation_non_tty_still_prints_registry() {
        // Regression guard: earlier behavior errored on non-TTY bare
        // invocation ("agent-setup requires an agent name ..."). The soft
        // revert prints the registry + usage hint and returns Ok(()) on
        // both TTY and non-TTY so piped/non-interactive callers get the
        // same friendly output.
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf()); // tty=false by default
        let mut out: Vec<u8> = Vec::new();
        run_with_env_to_writer(None, false, false, false, None, &env, &mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("aimx agent-setup <agent>"));
    }
}
