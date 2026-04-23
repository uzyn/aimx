//! Per-agent plugin installer for `aimx agent-setup`.
//!
//! Ships plugin/skill packages for supported agents (currently Claude Code)
//! bundled into the binary via `include_dir!`, and installs them into the
//! user's `$HOME`-based agent directory.

use crate::config::HookTemplateStdin;
use crate::hook::HookEvent;
use crate::hook_client::{TemplateCrudFallback, submit_template_create_via_daemon};
use crate::send_protocol::{
    TemplateCreateRequest, TemplateDeleteRequest, TemplateUpdateRequest, UdsTemplatePayload,
};
use crate::term;
use include_dir::{Dir, DirEntry, include_dir};
use std::ffi::OsString;
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
    /// Top-level config directory for the agent itself (e.g. `$HOME/.claude`).
    /// `detect_install_state` (Sprint 6 S6-1) uses this to distinguish
    /// "agent not installed on this machine" (directory missing) from
    /// "agent installed but aimx not wired in yet" (directory exists but
    /// `dest_template` does not). Always an ancestor of `dest_template`.
    pub agent_root_template: &'static str,
    /// Human-readable display name for TUI / summary output (e.g.
    /// "Claude Code"). Registry key stays the snake-case `name` field.
    pub display_name: &'static str,
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
    /// Canonical binary name probed on `$PATH` during `aimx agent-setup`
    /// to locate the agent's executable (e.g. `claude-code` → `claude`).
    /// `cmd[0]` of the registered template is the resolved path.
    pub canonical_binary: &'static str,
    /// Extra argv appended to `[<found_path>]` when building the
    /// template's `cmd` on `TEMPLATE-CREATE`. Empty by default — agents
    /// launched headlessly with piped stdin need nothing here.
    pub args: &'static [&'static str],
    /// `UdsTemplatePayload.params` — declared placeholder names the
    /// template's `cmd` may reference. Empty for the default `invoke-*`
    /// shape; present when the registry wants to accept MCP-bound params.
    pub params: &'static [&'static str],
    /// Stdin delivery mode for the hook child (`email`, `email_json`, or
    /// `none`). Every v1 agent takes the raw `.md` on stdin.
    pub stdin: HookTemplateStdin,
    /// Hard timeout in seconds for the hook child, within
    /// `[1, HOOK_TEMPLATE_TIMEOUT_SECS_MAX]`.
    pub timeout_secs: u32,
    /// Events the template may be wired to on hook creation. v1 agents
    /// are invoke-on-arrival, i.e. `on_receive` only; they fire nothing
    /// on outbound send.
    pub allowed_events: &'static [HookEvent],
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
            agent_root_template: "$HOME/.claude",
            display_name: "Claude Code",
            activation_hint: claude_code_hint,
            progressive_disclosure: true,
            canonical_binary: "claude",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
        },
        AgentSpec {
            name: "codex",
            source_subdir: "codex",
            dest_template: "$HOME/.codex/skills/aimx",
            agent_root_template: "$HOME/.codex",
            display_name: "Codex CLI",
            activation_hint: codex_hint,
            progressive_disclosure: true,
            canonical_binary: "codex",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
        },
        AgentSpec {
            name: "opencode",
            source_subdir: "opencode",
            dest_template: "$XDG_CONFIG_HOME/opencode/skills/aimx",
            agent_root_template: "$XDG_CONFIG_HOME/opencode",
            display_name: "OpenCode",
            activation_hint: opencode_hint,
            progressive_disclosure: false,
            canonical_binary: "opencode",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
        },
        AgentSpec {
            name: "gemini",
            source_subdir: "gemini",
            dest_template: "$HOME/.gemini/skills/aimx",
            agent_root_template: "$HOME/.gemini",
            display_name: "Gemini CLI",
            activation_hint: gemini_hint,
            progressive_disclosure: false,
            canonical_binary: "gemini",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
        },
        AgentSpec {
            name: "goose",
            source_subdir: "goose",
            // Goose discovers recipes by filename stem from
            // ~/.config/goose/recipes/. We install one file, not a
            // directory. Destination template points at the file itself.
            dest_template: "$XDG_CONFIG_HOME/goose/recipes",
            agent_root_template: "$XDG_CONFIG_HOME/goose",
            display_name: "Goose",
            activation_hint: goose_hint,
            progressive_disclosure: false,
            canonical_binary: "goose",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
        },
        AgentSpec {
            name: "openclaw",
            source_subdir: "openclaw",
            // OpenClaw scans ~/.openclaw/skills/<name>/SKILL.md. We ship a
            // skill-directory package (no flat SKILL.md at the root).
            dest_template: "$HOME/.openclaw/skills/aimx",
            agent_root_template: "$HOME/.openclaw",
            display_name: "OpenClaw",
            activation_hint: openclaw_hint,
            progressive_disclosure: true,
            canonical_binary: "openclaw",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
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
            agent_root_template: "$HOME/.hermes",
            display_name: "Hermes",
            activation_hint: hermes_hint,
            progressive_disclosure: true,
            canonical_binary: "hermes",
            args: &[],
            params: &[],
            stdin: HookTemplateStdin::Email,
            timeout_secs: 60,
            allowed_events: &[HookEvent::OnReceive],
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

/// Reasons `derive_template_name` refuses an input.
///
/// Sprint 5 §6.1 / §8.2: both parts of `invoke-<agent>-<username>` must
/// match `[a-z0-9-]+` because the template-name validator used by
/// `Config::load` rejects anything else. Usernames that fall outside the
/// charset can still own mailboxes (operators can hand-author templates
/// in `config.toml`), but they cannot use `agent-setup`'s template
/// registration because the derived name would fail validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateNameError {
    EmptyAgentSlug,
    EmptyUsername,
    InvalidAgentSlug(String),
    InvalidUsername(String),
}

impl std::fmt::Display for TemplateNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateNameError::EmptyAgentSlug => write!(f, "agent slug is empty"),
            TemplateNameError::EmptyUsername => write!(f, "username is empty"),
            TemplateNameError::InvalidAgentSlug(s) => write!(
                f,
                "agent slug '{s}' is not a valid template name component (must match [a-z0-9-]+)"
            ),
            TemplateNameError::InvalidUsername(s) => write!(
                f,
                "username '{s}' contains characters not valid for template names. \
                 Hand-author a template in config.toml."
            ),
        }
    }
}

impl std::error::Error for TemplateNameError {}

/// Derive the canonical template name `invoke-<agent>-<username>` per
/// PRD §6.6. Both parts must match `[a-z0-9-]+`. Rejects empty
/// components, uppercase letters, underscores, and non-ASCII characters.
///
/// `agent-setup` uses this helper both when registering a new template
/// (`TEMPLATE-CREATE`) and when re-detecting the binary path
/// (`TEMPLATE-UPDATE` on `--redetect`). Keeping one derivation
/// guarantees idempotence: re-running the command always targets the
/// same name.
pub fn derive_template_name(agent_slug: &str, username: &str) -> Result<String, TemplateNameError> {
    if agent_slug.is_empty() {
        return Err(TemplateNameError::EmptyAgentSlug);
    }
    if username.is_empty() {
        return Err(TemplateNameError::EmptyUsername);
    }
    if !is_valid_template_name_component(agent_slug) {
        return Err(TemplateNameError::InvalidAgentSlug(agent_slug.to_string()));
    }
    if !is_valid_template_name_component(username) {
        return Err(TemplateNameError::InvalidUsername(username.to_string()));
    }
    Ok(format!("invoke-{agent_slug}-{username}"))
}

fn is_valid_template_name_component(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Trait used to make installs testable without touching the real `$HOME`
/// or real uid.
pub trait AgentEnv {
    fn home_dir(&self) -> Option<PathBuf>;
    fn xdg_config_home(&self) -> Option<PathBuf>;
    fn is_root(&self) -> bool;
    fn is_stdin_tty(&self) -> bool;
    fn read_line(&self) -> io::Result<String>;
    /// The current user's Linux username (from `getpwuid(geteuid())`).
    /// Since `agent-setup` refuses root, this is always a real user.
    fn caller_username(&self) -> Option<String> {
        caller_username_from_euid()
    }
    /// Probe the caller's `$PATH` for `binary_name`. Default wraps the
    /// free-standing `probe_path` helper so production callers pick up
    /// the real `$PATH` without forcing tests to mutate process env.
    fn probe_binary(&self, binary_name: &str) -> Option<PathBuf> {
        probe_path(binary_name)
    }
    /// Submit a `TEMPLATE-CREATE` frame to the daemon. Default delegates
    /// to the UDS client; tests can override to avoid spinning up a
    /// socket.
    fn submit_template_create(
        &self,
        request: &TemplateCreateRequest,
    ) -> Result<(), TemplateCrudFallback> {
        submit_template_create_via_daemon(request)
    }
    /// Submit a `TEMPLATE-UPDATE` frame to the daemon. Default delegates
    /// to the UDS client; tests can override to avoid spinning up a
    /// socket.
    fn submit_template_update(
        &self,
        request: &TemplateUpdateRequest,
    ) -> Result<(), TemplateCrudFallback> {
        crate::hook_client::submit_template_update_via_daemon(request)
    }
    /// Submit a `TEMPLATE-DELETE` frame to the daemon. Default delegates
    /// to the UDS client; tests can override to exercise the
    /// socket-missing / NOTFOUND branches without a daemon.
    fn submit_template_delete(
        &self,
        request: &TemplateDeleteRequest,
    ) -> Result<(), TemplateCrudFallback> {
        crate::hook_client::submit_template_delete_via_daemon(request)
    }
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

/// Wrapper around an inner [`AgentEnv`] that shadows `home_dir()` and
/// `xdg_config_home()` with a fixed home path. Used by the Sprint 6
/// `--dangerously-allow-root` code path (FR-5.1.a): with the flag set,
/// `home_dir()` resolves to the `root` passwd entry's `pw_dir` (not `$HOME`,
/// which can be unset under `sudo -H`), and XDG defaults to `<home>/.config`.
/// All other methods (root check, TTY detection, UDS submits) pass through
/// to the wrapped env unchanged.
pub struct OverrideHomeEnv<'a> {
    inner: &'a dyn AgentEnv,
    home: PathBuf,
}

impl<'a> OverrideHomeEnv<'a> {
    pub fn new(inner: &'a dyn AgentEnv, home: PathBuf) -> Self {
        Self { inner, home }
    }
}

impl<'a> AgentEnv for OverrideHomeEnv<'a> {
    fn home_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }

    fn xdg_config_home(&self) -> Option<PathBuf> {
        // Intentionally ignore the ambient `$XDG_CONFIG_HOME` under the
        // root override: the regular user's XDG dir doesn't apply when
        // we're writing into `/root/.config/`.
        None
    }

    fn is_root(&self) -> bool {
        self.inner.is_root()
    }

    fn is_stdin_tty(&self) -> bool {
        self.inner.is_stdin_tty()
    }

    fn read_line(&self) -> io::Result<String> {
        self.inner.read_line()
    }

    fn caller_username(&self) -> Option<String> {
        self.inner.caller_username()
    }

    fn probe_binary(&self, binary_name: &str) -> Option<PathBuf> {
        self.inner.probe_binary(binary_name)
    }

    fn submit_template_create(
        &self,
        request: &TemplateCreateRequest,
    ) -> Result<(), TemplateCrudFallback> {
        self.inner.submit_template_create(request)
    }

    fn submit_template_update(
        &self,
        request: &TemplateUpdateRequest,
    ) -> Result<(), TemplateCrudFallback> {
        self.inner.submit_template_update(request)
    }

    fn submit_template_delete(
        &self,
        request: &TemplateDeleteRequest,
    ) -> Result<(), TemplateCrudFallback> {
        self.inner.submit_template_delete(request)
    }
}

/// Resolve the caller's Linux username via `getpwuid(geteuid())`. Returns
/// `None` if the euid does not map to a passwd entry. `agent-setup`
/// refuses root up front (PRD §6.6 + §8.2), so in production this always
/// resolves to a real, non-root username. Used as the `run_as` / username
/// component of the derived template name.
///
/// Thin wrapper over [`crate::uds_authz::lookup_username`] so the two
/// callers (UDS authz cache and agent-setup) share one `getpwuid` helper.
pub fn caller_username_from_euid() -> Option<String> {
    // SAFETY: `geteuid` is a bare syscall with no preconditions.
    let uid = unsafe { libc::geteuid() };
    crate::uds_authz::lookup_username(uid)
}

/// Resolve a Linux user's home directory via `getpwnam(3)`. Used by
/// `--dangerously-allow-root` (FR-5.1.a) to look up `/root` — or whatever
/// the local `root` account's `pw_dir` is — without relying on `$HOME`,
/// which can be unset or stale under `sudo -H`.
///
/// Returns `None` when the username has no matching passwd entry, when
/// the name contains an interior NUL byte, or when `pw_dir` is empty.
pub fn home_dir_for_user(username: &str) -> Option<PathBuf> {
    use std::ffi::CStr;
    let cname = std::ffi::CString::new(username).ok()?;
    // SAFETY: `getpwnam` reads a process-global static; we copy the
    // `pw_dir` field (a `*mut c_char`) into an owned `PathBuf` before any
    // subsequent getpw* call could invalidate the static.
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let dir_ptr = unsafe { (*pw).pw_dir };
    if dir_ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(dir_ptr) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Walk the caller's `$PATH` in order looking for an executable named
/// `binary_name`. Returns the canonical path of the first match.
///
/// `$PATH` is read via `env::var_os` and split on POSIX `:`. Each entry is
/// joined with `binary_name`, `stat(2)`'d (empty entries, missing files,
/// and non-regular files are skipped silently), and checked for
/// `access(X_OK)` via `nix::unistd::access`. The first entry that passes
/// wins; `canonicalize` resolves symlinks so callers always see the real
/// on-disk path.
///
/// Returns `None` when `$PATH` is unset/empty or no entry contains an
/// executable match.
///
/// Pure helper: no I/O beyond `stat`/`access`/`canonicalize`. Safe to
/// call on the fast-refusal path before the daemon is reached.
pub fn probe_path(binary_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    probe_path_in(binary_name, &path)
}

/// Testable core of [`probe_path`]. Exposed with `pub(crate)` so unit
/// tests can inject a controlled `PATH` value without mutating
/// process-global `env::set_var` state.
pub(crate) fn probe_path_in(binary_name: &str, path_var: &OsString) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    if binary_name.is_empty() {
        return None;
    }

    // Split on `:` at the byte level so a non-UTF-8 $PATH entry doesn't
    // get silently dropped. POSIX $PATH entries are byte strings and can
    // legally contain any byte except `:` and `\0`.
    let bytes = path_var.as_bytes();
    for entry in bytes.split(|b| *b == b':') {
        if entry.is_empty() {
            // POSIX reserves an empty `$PATH` entry as the current
            // working directory. We treat it the same as a missing
            // directory (skip) to avoid surprising the operator with
            // a CWD-resolved match.
            continue;
        }
        let entry_os = std::ffi::OsStr::from_bytes(entry);
        let candidate = Path::new(entry_os).join(binary_name);

        let meta = match std::fs::metadata(&candidate) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if nix::unistd::access(&candidate, nix::unistd::AccessFlags::X_OK).is_err() {
            continue;
        }
        // `canonicalize` resolves any symlinks so the stored `cmd[0]` is
        // the real on-disk path. If canonicalization fails (rare — the
        // path was just `stat`'d), fall back to the joined candidate.
        return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
    }

    None
}

/// Options controlling a single `agent-setup` invocation.
pub struct InstallOptions<'a> {
    pub force: bool,
    pub print: bool,
    pub data_dir: Option<&'a Path>,
    /// `--no-template`: install plugin files only; skip probe + UDS
    /// `TEMPLATE-CREATE`. Mutually meaningful with `--redetect` (the CLI
    /// rejects the combination).
    pub no_template: bool,
    /// `--redetect`: re-probe `$PATH` and submit `TEMPLATE-UPDATE`
    /// instead of `TEMPLATE-CREATE`, pointing the existing template at
    /// the new binary path.
    pub redetect: bool,
}

/// Resolve a destination template against the environment. Substitutes
/// `$HOME` and `$XDG_CONFIG_HOME`.
pub fn resolve_dest(template: &str, env: &dyn AgentEnv) -> Result<PathBuf, String> {
    let home = env.home_dir().ok_or_else(|| {
        "HOME is not set; agent-setup writes to the user's home directory".to_string()
    })?;
    Ok(resolve_template_in_home(
        template,
        &home,
        env.xdg_config_home(),
    ))
}

/// Substitute `$HOME` / `$XDG_CONFIG_HOME` in a template against an explicit
/// home path. Sprint 6 detection (`detect_install_state`) uses this helper
/// to resolve paths against a caller-chosen home — for
/// `--dangerously-allow-root` that's `/root`, not the ambient env's HOME.
/// When `xdg` is `None`, defaults to `<home>/.config` per the XDG Base
/// Directory spec.
pub fn resolve_template_in_home(template: &str, home: &Path, xdg: Option<PathBuf>) -> PathBuf {
    let xdg_dir = xdg.unwrap_or_else(|| home.join(".config"));
    let substituted = template
        .replace("$XDG_CONFIG_HOME", &xdg_dir.to_string_lossy())
        .replace("$HOME", &home.to_string_lossy());
    PathBuf::from(substituted)
}

/// Per-agent install state used to render the Sprint 6 checkbox TUI.
///
/// - `InstalledWired`: the plugin destination path we'd write to already
///   exists on disk — an earlier `aimx agent-setup <name>` has landed
///   plugin files there, so aimx is wired into this agent. Rendered
///   dim + default-unselected in the TUI (`[x] (already wired)`).
/// - `InstalledNotWired`: the agent's own config directory exists but no
///   aimx plugin files do. The agent is present on the machine; aimx is
///   not wired in yet. Rendered default-selected (`[ ]`).
/// - `NotInstalled`: the agent's own config directory is missing. The
///   agent itself isn't installed on this machine. Rendered dim +
///   non-selectable (`[-] (not detected)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    InstalledWired,
    InstalledNotWired,
    NotInstalled,
}

/// Detect the install / wired state of one agent against a concrete home
/// directory. Sprint 6 S6-1 + FR-5.2.
///
/// Rules:
/// 1. If `dest_template` (resolved under `home`) exists → `InstalledWired`.
/// 2. Else, if `agent_root_template` (resolved under `home`) exists →
///    `InstalledNotWired` (agent present, aimx not yet wired).
/// 3. Else → `NotInstalled`.
///
/// `xdg` is threaded through so agents whose paths use `$XDG_CONFIG_HOME`
/// (opencode, goose) resolve identically to how the installer would
/// resolve them. `None` falls back to `<home>/.config`.
pub fn detect_install_state(spec: &AgentSpec, home: &Path, xdg: Option<PathBuf>) -> InstallState {
    let dest = resolve_template_in_home(spec.dest_template, home, xdg.clone());
    if dest.exists() {
        return InstallState::InstalledWired;
    }
    let root = resolve_template_in_home(spec.agent_root_template, home, xdg);
    if root.exists() {
        return InstallState::InstalledNotWired;
    }
    InstallState::NotInstalled
}

/// Parameters for one `aimx agent-setup` invocation. Carries everything
/// `run` / `run_with_env` / `run_with_env_to_writer` need so these
/// entry-point signatures don't keep growing each time a new CLI flag
/// lands. Short-lived, borrowed-`data_dir`; not `Clone` by design —
/// callers should not reuse the same `RunOpts` across invocations.
pub struct RunOpts<'a> {
    pub agent: Option<String>,
    pub list: bool,
    pub force: bool,
    pub print: bool,
    pub no_template: bool,
    pub redetect: bool,
    /// FR-5.5 — force the plain registry-dump path instead of the Sprint 6
    /// interactive TUI when no agent argument is passed. Safe to use in
    /// scripts, non-TTY environments, and tests.
    pub no_interactive: bool,
    /// FR-5.1.a — bypass the root-refusal check and resolve `$HOME` to
    /// `/root`. Applies uniformly to the TUI, per-agent runs, and
    /// `--no-interactive`. Drop-through from `aimx setup` never sets
    /// this; the flag is operator-opt-in only.
    pub dangerously_allow_root: bool,
    pub data_dir: Option<&'a Path>,
}

/// FR-5.1 root-refusal message. Names both escape hatches: re-run as a
/// regular user (`sudo -u <user> aimx agent-setup`) **or** pass
/// `--dangerously-allow-root` for single-user root-login VPS setups that
/// genuinely want aimx wired into root's home.
pub const ROOT_REFUSAL_MESSAGE: &str = "agent-setup is a per-user operation and refuses to run as root by default.\n\
     \n\
     Re-run as your regular user:\n\
     \x20\x20sudo -u <user> aimx agent-setup\n\
     \n\
     Or, on a single-user root-login VPS that has no separate operator account,\n\
     pass --dangerously-allow-root to wire aimx into /root's home:\n\
     \x20\x20aimx agent-setup --dangerously-allow-root";

/// Entry point called from `main.rs`.
pub fn run(opts: RunOpts<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let env = RealAgentEnv;
    match run_with_env(opts, &env) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Daemon-down → exit 2 (matches `aimx send`'s socket-missing
            // convention, §6.6 AC); binary-missing → exit 3. Other errors
            // fall through to the caller's default exit code.
            if let Some(code) = e.downcast_ref::<AgentSetupExitCode>() {
                eprintln!("{}", code.message());
                std::process::exit(code.code());
            }
            Err(e)
        }
    }
}

pub fn run_with_env(
    opts: RunOpts<'_>,
    env: &dyn AgentEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_to_writer(opts, env, &mut io::stdout())
}

/// Testable core of `run_with_env`: writes install output, `--list` output,
/// or the bare-invocation registry dump (plus usage-hint footer) to `out`
/// rather than real stdout so tests can capture and assert on it.
pub fn run_with_env_to_writer(
    opts: RunOpts<'_>,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    if opts.list {
        print_registry_to_writer(env, out)?;
        return Ok(());
    }

    // FR-5.1 / FR-5.1.a — root gate. Applies uniformly to the TUI, per-agent
    // runs, and `--no-interactive`. With `--dangerously-allow-root`, swap in
    // an `OverrideHomeEnv` that points at `/root` (via `getpwnam("root")`) so
    // every downstream path — detection, `resolve_dest`, the TUI, the
    // template preview — sees root's home, not the ambient `$HOME`.
    if env.is_root() {
        if !opts.dangerously_allow_root {
            return Err(ROOT_REFUSAL_MESSAGE.into());
        }
        let root_home = home_dir_for_user("root").unwrap_or_else(|| PathBuf::from("/root"));
        let override_env = OverrideHomeEnv::new(env, root_home);
        return run_with_env_to_writer_inner(opts, &override_env, out);
    }

    run_with_env_to_writer_inner(opts, env, out)
}

/// Dispatch the non-`--list` branches after the root gate has already been
/// applied. Split out so `--dangerously-allow-root` can swap in an
/// `OverrideHomeEnv` wrapper around the ambient env before we hit any
/// path-resolving code.
fn run_with_env_to_writer_inner(
    opts: RunOpts<'_>,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    // Defense in depth: clap's `conflicts_with = "redetect"` on
    // `no_template` (see `cli.rs`) already rejects this combination at
    // parse time with a standard clap usage-hint. This runtime check is
    // kept so direct library callers of `run_with_env` that skip clap
    // still fail loudly rather than silently picking one branch.
    if opts.no_template && opts.redetect {
        return Err("--no-template and --redetect are mutually exclusive; \
             --no-template skips template registration entirely, --redetect \
             updates an existing template"
            .into());
    }

    let install_opts = InstallOptions {
        force: opts.force,
        print: opts.print,
        data_dir: opts.data_dir,
        no_template: opts.no_template,
        redetect: opts.redetect,
    };

    let spec = match opts.agent.as_deref() {
        Some(name) => find_agent(name).ok_or_else(|| {
            format!("unknown agent '{name}'; run `aimx agent-setup --list` to see supported agents")
        })?,
        None => {
            // Bare invocation. With `--no-interactive` (or a non-TTY
            // stdout such as a pipe), fall back to the plain registry
            // dump so scripts get a deterministic listing. Otherwise,
            // launch the Sprint 6 checkbox TUI per FR-5.5.
            if opts.no_interactive || !is_stdout_tty() {
                print_registry_to_writer(env, out)?;
                writeln!(
                    out,
                    "Run `aimx agent-setup <agent>` to install one of the agents \
                     above, or `aimx agent-setup --list` to print this list again."
                )?;
                return Ok(());
            }
            return crate::agent_tui::run_tui(&opts, env, out);
        }
    };

    install_to_writer(spec, &install_opts, env, out)
}

fn is_stdout_tty() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal()
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
/// Handles `--print` (dry run; emits file list + activation hint + the
/// rendered template TOML the install WOULD register) and the normal
/// install path (lays files down under `dest_template`, then prints the
/// activation hint and the template-registration status line per
/// PRD §6.6).
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

        // Render the template TOML `agent-setup` WOULD submit. Uses the
        // caller's real username so the derived name matches what the
        // daemon would store. If the username can't be resolved or does
        // not pass the `[a-z0-9-]+` charset (PRD §8.2), fall back to a
        // `<username>` placeholder and explain inline.
        writeln!(out, "=== template ===")?;
        match print_template_preview(spec, env) {
            Ok(toml_body) => writeln!(out, "{toml_body}")?,
            Err(e) => writeln!(out, "<template could not be rendered: {e}>")?,
        }
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

    // Template registration (Sprint 6, PRD §6.6). One status line,
    // picked from the three cases in the sprint spec.
    writeln!(out)?;
    let outcome = register_template(spec, opts, env);
    match outcome {
        TemplateOutcome::Registered { name, path } => {
            writeln!(
                out,
                "{} {} (cmd: {}, run_as: {})",
                term::success("Template"),
                term::highlight(&name),
                term::highlight(&path.to_string_lossy()),
                term::highlight(&caller_username_for_display(env)),
            )?;
            writeln!(out, "  registered via aimx-socket.")?;
        }
        TemplateOutcome::Updated { name, path } => {
            writeln!(
                out,
                "{} {} re-pointed at {}",
                term::success("Template"),
                term::highlight(&name),
                term::highlight(&path.to_string_lossy()),
            )?;
        }
        TemplateOutcome::Skipped => {
            writeln!(
                out,
                "{}",
                term::info("Skipped template registration (--no-template).")
            )?;
        }
        TemplateOutcome::AlreadyExists { name } => {
            writeln!(
                out,
                "{} {} already exists. Use 'aimx agent-setup {} --redetect' to update it.",
                term::warn("Template"),
                term::highlight(&name),
                spec.name,
            )?;
        }
        TemplateOutcome::Failed { reason, exit_code } => {
            let msg = format!("Template registration failed: {reason}");
            if let Some(code) = exit_code {
                // Error routed via `AgentSetupExitCode`; `run()` prints
                // `msg` to stderr before `process::exit(code)`. Emitting
                // to `out` too would duplicate the line across streams.
                return Err(Box::new(AgentSetupExitCode::from_code(code, msg)));
            }
            writeln!(out, "{}", term::error(&msg))?;
        }
    }

    Ok(())
}

/// Compose the `UdsTemplatePayload` the current invocation would submit
/// and return it rendered as TOML. Used by `--print` to give operators
/// a dry-run view of the template (PRD §6.6 + sprint S6-3).
fn print_template_preview(spec: &AgentSpec, env: &dyn AgentEnv) -> Result<String, String> {
    let username = env.caller_username().unwrap_or_else(|| "<username>".into());
    // Allow the placeholder-username case through render so `--print` on
    // a minimal container image still emits a syntactically valid TOML
    // preview. The real register path enforces charset via
    // `derive_template_name`.
    let name = match derive_template_name(spec.name, &username) {
        Ok(n) => n,
        Err(_) => format!("invoke-{}-<username>", spec.name),
    };
    let payload = build_template_payload_preview(spec, &name, &username, env);
    crate::send_protocol::render_template_payload(&payload).map_err(|e| e.to_string())
}

/// Outcome categories for the template-registration status line printed
/// after a successful plugin install. Every case maps to exactly one
/// line of on-screen output per Sprint 6 §S6-4.
enum TemplateOutcome {
    /// Happy path: `TEMPLATE-CREATE` accepted.
    Registered { name: String, path: PathBuf },
    /// `--redetect` succeeded; the template now points at `path`.
    Updated { name: String, path: PathBuf },
    /// `--no-template` was set; nothing submitted.
    Skipped,
    /// Daemon reported `ECONFLICT` on CREATE — the template already
    /// exists. Operator should re-run with `--redetect` (PRD §6.6).
    AlreadyExists { name: String },
    /// Every other failure: probe miss, daemon down, validation, etc.
    /// `exit_code` propagates as the CLI exit code when set.
    Failed {
        reason: String,
        exit_code: Option<i32>,
    },
}

/// Drive the probe + UDS submit flow. Pure w.r.t. the filesystem: caller
/// decides how to render `TemplateOutcome` to stdout. Extracted so
/// `install_to_writer` stays readable and unit tests can drive it
/// without also asserting on the surrounding plugin-install copy.
fn register_template(
    spec: &AgentSpec,
    opts: &InstallOptions,
    env: &dyn AgentEnv,
) -> TemplateOutcome {
    if opts.no_template {
        return TemplateOutcome::Skipped;
    }

    let Some(username) = env.caller_username() else {
        return TemplateOutcome::Failed {
            reason: "Could not resolve the caller's username from getpwuid(geteuid()).".to_string(),
            exit_code: Some(1),
        };
    };

    let name = match derive_template_name(spec.name, &username) {
        Ok(n) => n,
        Err(e) => {
            return TemplateOutcome::Failed {
                reason: e.to_string(),
                exit_code: Some(1),
            };
        }
    };

    let path = match env.probe_binary(spec.canonical_binary) {
        Some(p) => p,
        None => {
            let bin = spec.canonical_binary;
            return TemplateOutcome::Failed {
                reason: format!(
                    "Could not find '{bin}' in $PATH. Install it, then re-run 'aimx agent-setup {}'.",
                    spec.name
                ),
                exit_code: Some(3),
            };
        }
    };

    let payload = build_template_payload_with_path(spec, &name, &username, &path);

    if opts.redetect {
        let request = TemplateUpdateRequest {
            name: name.clone(),
            payload,
        };
        match env.submit_template_update(&request) {
            Ok(()) => TemplateOutcome::Updated { name, path },
            Err(TemplateCrudFallback::SocketMissing) => TemplateOutcome::Failed {
                reason: format!(
                    "aimx serve is not running; start it with 'sudo systemctl start aimx' \
                     and re-run 'aimx agent-setup {} --redetect'.",
                    spec.name
                ),
                exit_code: Some(2),
            },
            Err(TemplateCrudFallback::Daemon { code, reason }) => {
                if code == "ENOENT" || code == "NOTFOUND" {
                    TemplateOutcome::Failed {
                        reason: format!(
                            "Template '{name}' does not exist yet. Run 'aimx agent-setup {}' \
                             without --redetect first.",
                            spec.name
                        ),
                        exit_code: Some(1),
                    }
                } else if code == "EACCES" {
                    TemplateOutcome::Failed {
                        reason: format!(
                            "Permission denied updating template '{name}' (EACCES): {reason}"
                        ),
                        exit_code: Some(1),
                    }
                } else {
                    TemplateOutcome::Failed {
                        reason: format!("[{code}] {reason}"),
                        exit_code: Some(1),
                    }
                }
            }
            Err(TemplateCrudFallback::Local(msg)) => TemplateOutcome::Failed {
                reason: msg,
                exit_code: Some(1),
            },
        }
    } else {
        let request = TemplateCreateRequest { payload };
        match env.submit_template_create(&request) {
            Ok(()) => TemplateOutcome::Registered { name, path },
            Err(TemplateCrudFallback::SocketMissing) => TemplateOutcome::Failed {
                reason: format!(
                    "aimx serve is not running; start it with 'sudo systemctl start aimx' \
                     and re-run 'aimx agent-setup {}'.",
                    spec.name
                ),
                exit_code: Some(2),
            },
            Err(TemplateCrudFallback::Daemon { code, reason }) => {
                if code == "ECONFLICT" {
                    TemplateOutcome::AlreadyExists { name }
                } else {
                    TemplateOutcome::Failed {
                        reason: format!("[{code}] {reason}"),
                        exit_code: Some(1),
                    }
                }
            }
            Err(TemplateCrudFallback::Local(msg)) => TemplateOutcome::Failed {
                reason: msg,
                exit_code: Some(1),
            },
        }
    }
}

fn build_template_payload_with_path(
    spec: &AgentSpec,
    name: &str,
    username: &str,
    path: &Path,
) -> UdsTemplatePayload {
    let mut cmd = Vec::with_capacity(1 + spec.args.len());
    cmd.push(path.to_string_lossy().into_owned());
    for arg in spec.args {
        cmd.push((*arg).to_string());
    }
    UdsTemplatePayload {
        name: name.to_string(),
        description: format!(
            "aimx agent-setup: invoke {} headlessly for {}",
            spec.name, username
        ),
        cmd,
        params: spec.params.iter().map(|p| (*p).to_string()).collect(),
        stdin: spec.stdin,
        run_as: username.to_string(),
        timeout_secs: spec.timeout_secs,
        allowed_events: spec.allowed_events.to_vec(),
    }
}

/// Build a payload for the `--print` preview path. When `$PATH` has no
/// match, the probed path is unknown — we substitute `<binary>` as a
/// placeholder so the TOML stays self-describing.
fn build_template_payload_preview(
    spec: &AgentSpec,
    name: &str,
    username: &str,
    env: &dyn AgentEnv,
) -> UdsTemplatePayload {
    let path = env
        .probe_binary(spec.canonical_binary)
        .unwrap_or_else(|| PathBuf::from(format!("<{}>", spec.canonical_binary)));
    build_template_payload_with_path(spec, name, username, &path)
}

/// Resolve the caller's username for display purposes, falling back to
/// a literal `<username>` placeholder when `getpwuid` returns `None`.
fn caller_username_for_display(env: &dyn AgentEnv) -> String {
    env.caller_username().unwrap_or_else(|| "<username>".into())
}

/// Wrapper error used to carry a specific exit code out of
/// `agent_setup::run` without losing the user-facing message. `main.rs`
/// exits with the contained code so the CLI matches the documented
/// convention (daemon-down → 2, binary-missing → 3).
#[derive(Debug)]
pub struct AgentSetupExitCode {
    code: i32,
    message: String,
}

impl AgentSetupExitCode {
    fn from_code(code: i32, message: String) -> Self {
        Self { code, message }
    }

    pub fn code(&self) -> i32 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AgentSetupExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AgentSetupExitCode {}

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

    /// Behavior a `MockEnv` test harness can inject for the Sprint 6 probe
    /// + UDS path. `None` on `probe_result` means "act like the binary is
    ///   missing from `$PATH`". `submit_*_result` defaults to `Ok(())`.
    struct TemplateStub {
        probe_result: Option<PathBuf>,
        submit_create_result: Result<(), TemplateCrudFallback>,
        submit_update_result: Result<(), TemplateCrudFallback>,
        submit_create_calls: RefCell<Vec<TemplateCreateRequest>>,
        submit_update_calls: RefCell<Vec<TemplateUpdateRequest>>,
    }

    impl Default for TemplateStub {
        fn default() -> Self {
            Self {
                // By default the canonical binary resolves to a synthetic
                // path so the happy-path tests don't need to touch
                // `$PATH`. Tests that exercise probe-miss override this.
                probe_result: Some(PathBuf::from("/mock/bin/agent")),
                submit_create_result: Ok(()),
                submit_update_result: Ok(()),
                submit_create_calls: RefCell::new(Vec::new()),
                submit_update_calls: RefCell::new(Vec::new()),
            }
        }
    }

    struct MockEnv {
        home: PathBuf,
        xdg: Option<PathBuf>,
        root: bool,
        tty: bool,
        responses: RefCell<Vec<String>>,
        username: Option<String>,
        template: TemplateStub,
    }

    impl MockEnv {
        fn new(home: PathBuf) -> Self {
            Self {
                home,
                xdg: None,
                root: false,
                tty: false,
                responses: RefCell::new(Vec::new()),
                username: Some("sam".to_string()),
                template: TemplateStub::default(),
            }
        }

        fn with_template_stub(mut self, stub: TemplateStub) -> Self {
            self.template = stub;
            self
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
        fn caller_username(&self) -> Option<String> {
            self.username.clone()
        }
        fn probe_binary(&self, _binary_name: &str) -> Option<PathBuf> {
            self.template.probe_result.clone()
        }
        fn submit_template_create(
            &self,
            request: &TemplateCreateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            self.template
                .submit_create_calls
                .borrow_mut()
                .push(request.clone());
            match &self.template.submit_create_result {
                Ok(()) => Ok(()),
                Err(e) => Err(clone_fallback(e)),
            }
        }
        fn submit_template_update(
            &self,
            request: &TemplateUpdateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            self.template
                .submit_update_calls
                .borrow_mut()
                .push(request.clone());
            match &self.template.submit_update_result {
                Ok(()) => Ok(()),
                Err(e) => Err(clone_fallback(e)),
            }
        }
        fn submit_template_delete(
            &self,
            _request: &TemplateDeleteRequest,
        ) -> Result<(), TemplateCrudFallback> {
            // Not exercised in agent-setup tests; agent-cleanup covers
            // the delete path in its own module tests.
            Err(TemplateCrudFallback::Local(
                "submit_template_delete not used in agent-setup tests".into(),
            ))
        }
    }

    fn clone_fallback(e: &TemplateCrudFallback) -> TemplateCrudFallback {
        match e {
            TemplateCrudFallback::SocketMissing => TemplateCrudFallback::SocketMissing,
            TemplateCrudFallback::Daemon { code, reason } => TemplateCrudFallback::Daemon {
                code: code.clone(),
                reason: reason.clone(),
            },
            TemplateCrudFallback::Local(s) => TemplateCrudFallback::Local(s.clone()),
        }
    }

    /// Test-scope wrapper matching the pre-Sprint-6 `run_with_env`
    /// signature so legacy tests keep their intent (plugin-install
    /// behavior). Every legacy caller exercised the install + plugin
    /// side of the flow; we default `no_template=true` so these tests
    /// don't need the daemon stub for assertions about files on disk.
    fn run_with_env(
        agent: Option<String>,
        list: bool,
        force: bool,
        print: bool,
        data_dir: Option<&Path>,
        env: &dyn AgentEnv,
    ) -> Result<(), Box<dyn std::error::Error>> {
        super::run_with_env(
            super::RunOpts {
                agent,
                list,
                force,
                print,
                no_template: true,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir,
            },
            env,
        )
    }

    /// Same as above for the writer-capturing entry point.
    fn run_with_env_to_writer(
        agent: Option<String>,
        list: bool,
        force: bool,
        print: bool,
        data_dir: Option<&Path>,
        env: &dyn AgentEnv,
        out: &mut dyn Write,
    ) -> Result<(), Box<dyn std::error::Error>> {
        super::run_with_env_to_writer(
            super::RunOpts {
                agent,
                list,
                force,
                print,
                no_template: true,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir,
            },
            env,
            out,
        )
    }

    #[test]
    fn derive_template_name_happy_path() {
        assert_eq!(
            derive_template_name("claude-code", "sam").unwrap(),
            "invoke-claude-code-sam"
        );
        assert_eq!(
            derive_template_name("codex", "alice").unwrap(),
            "invoke-codex-alice"
        );
    }

    #[test]
    fn derive_template_name_rejects_uppercase_username() {
        let err = derive_template_name("claude-code", "Sam").unwrap_err();
        assert!(matches!(err, TemplateNameError::InvalidUsername(ref s) if s == "Sam"));
        assert!(err.to_string().contains("Hand-author"));
    }

    #[test]
    fn derive_template_name_rejects_underscore_in_username() {
        let err = derive_template_name("claude-code", "deploy_user").unwrap_err();
        assert!(matches!(err, TemplateNameError::InvalidUsername(_)));
    }

    #[test]
    fn derive_template_name_rejects_empty_username() {
        let err = derive_template_name("claude-code", "").unwrap_err();
        assert_eq!(err, TemplateNameError::EmptyUsername);
    }

    #[test]
    fn derive_template_name_rejects_empty_agent_slug() {
        let err = derive_template_name("", "sam").unwrap_err();
        assert_eq!(err, TemplateNameError::EmptyAgentSlug);
    }

    #[test]
    fn derive_template_name_rejects_invalid_agent_slug() {
        let err = derive_template_name("Claude_Code", "sam").unwrap_err();
        assert!(matches!(err, TemplateNameError::InvalidAgentSlug(_)));
    }

    #[test]
    fn derive_template_name_rejects_non_ascii_username() {
        let err = derive_template_name("claude-code", "samé").unwrap_err();
        assert!(matches!(err, TemplateNameError::InvalidUsername(_)));
    }

    #[test]
    fn registry_contains_claude_code() {
        assert!(find_agent("claude-code").is_some());
        assert!(find_agent("not-a-real-agent").is_none());
    }

    // ----- Sprint 6 S6-1: detect_install_state -------------------------------

    #[test]
    fn detect_install_state_not_installed_on_empty_home() {
        // Pristine tempdir → every agent's root dir is missing.
        let tmp = TempDir::new().unwrap();
        for spec in registry() {
            let state = detect_install_state(spec, tmp.path(), None);
            assert_eq!(
                state,
                InstallState::NotInstalled,
                "agent {} must be NotInstalled on an empty home",
                spec.name
            );
        }
    }

    #[test]
    fn detect_install_state_installed_not_wired_when_agent_root_exists() {
        // Create `~/.claude` but not the plugin subdir → Claude Code is
        // installed, aimx plugin is not wired yet.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let spec = find_agent("claude-code").unwrap();
        let state = detect_install_state(spec, tmp.path(), None);
        assert_eq!(state, InstallState::InstalledNotWired);
    }

    #[test]
    fn detect_install_state_installed_wired_when_dest_exists() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude").join("plugins").join("aimx")).unwrap();
        let spec = find_agent("claude-code").unwrap();
        let state = detect_install_state(spec, tmp.path(), None);
        assert_eq!(state, InstallState::InstalledWired);
    }

    #[test]
    fn detect_install_state_respects_custom_xdg_home() {
        // opencode uses `$XDG_CONFIG_HOME/opencode/...`. Creating only the
        // default `<home>/.config/opencode` path is not enough when an
        // explicit XDG override points elsewhere — detection must follow
        // the override.
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("custom-xdg");
        std::fs::create_dir_all(xdg.join("opencode")).unwrap();
        let spec = find_agent("opencode").unwrap();
        let state_with_xdg = detect_install_state(spec, tmp.path(), Some(xdg.clone()));
        assert_eq!(state_with_xdg, InstallState::InstalledNotWired);

        // Without the override, `<home>/.config/opencode` is missing, so
        // the same spec reports NotInstalled.
        let state_no_xdg = detect_install_state(spec, tmp.path(), None);
        assert_eq!(state_no_xdg, InstallState::NotInstalled);
    }

    #[test]
    fn detect_install_state_flags_goose_recipe_file() {
        // Goose's dest_template points at a directory
        // (`$XDG_CONFIG_HOME/goose/recipes`). Creating the directory is
        // what "aimx has been wired" looks like because that's the path
        // `agent_setup` creates / writes into.
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join(".config");
        std::fs::create_dir_all(xdg.join("goose").join("recipes")).unwrap();
        let spec = find_agent("goose").unwrap();
        let state = detect_install_state(spec, tmp.path(), Some(xdg));
        assert_eq!(state, InstallState::InstalledWired);
    }

    // ----- Sprint 6 S6-2: OverrideHomeEnv / root override --------------------

    #[test]
    fn override_home_env_replaces_home_only() {
        let tmp = TempDir::new().unwrap();
        let inner = MockEnv::new(tmp.path().to_path_buf());
        let override_home = tmp.path().join("fake-root-home");
        std::fs::create_dir_all(&override_home).unwrap();
        let env = OverrideHomeEnv::new(&inner, override_home.clone());
        assert_eq!(env.home_dir(), Some(override_home.clone()));
        // XDG always resolves to `<override>/.config` under the root
        // override; ambient `$XDG_CONFIG_HOME` never leaks through.
        assert!(env.xdg_config_home().is_none());
        // Non-home methods still delegate to the inner env.
        assert!(!env.is_root());
        assert!(!env.is_stdin_tty());
    }

    #[test]
    fn override_home_env_detection_follows_override() {
        // Build a home tree under `override_home/.claude` and assert
        // detection reports InstalledNotWired when resolved through an
        // OverrideHomeEnv pointing at that tree — even though the inner
        // env's home is a different (empty) directory.
        let tmp = TempDir::new().unwrap();
        let inner_home = tmp.path().join("regular-user");
        std::fs::create_dir_all(&inner_home).unwrap();
        let root_home = tmp.path().join("fake-root");
        std::fs::create_dir_all(root_home.join(".claude")).unwrap();

        let inner = MockEnv::new(inner_home);
        let env = OverrideHomeEnv::new(&inner, root_home.clone());
        let home = env.home_dir().unwrap();
        let spec = find_agent("claude-code").unwrap();
        let state = detect_install_state(spec, &home, env.xdg_config_home());
        assert_eq!(state, InstallState::InstalledNotWired);
    }

    // ----- Sprint 6 S6-1/S6-2: root-refusal gate -----------------------------

    #[test]
    fn run_with_env_refuses_root_without_flag() {
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        env.root = true;
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: true,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refuses to run as root"),
            "refusal must mention root refusal: {msg}"
        );
        assert!(
            msg.contains("sudo -u <user>"),
            "refusal must name sudo -u escape hatch: {msg}"
        );
        assert!(
            msg.contains("--dangerously-allow-root"),
            "refusal must name the flag: {msg}"
        );
    }

    #[test]
    fn run_with_env_allows_root_with_dangerous_flag() {
        // `--dangerously-allow-root` bypasses the refusal and routes
        // detection through `/root` (or whatever the `root` account's
        // passwd entry exposes). We use `--list` here to avoid needing a
        // real install target — the gate runs before `--list` only when
        // no agent is passed AND the list flag is off. So instead we
        // drive the list-equivalent: pass a bogus agent name and expect
        // the "unknown agent" error (which only fires AFTER the root
        // gate). Seeing that error means the gate let us through.
        let tmp = TempDir::new().unwrap();
        let mut env = MockEnv::new(tmp.path().to_path_buf());
        env.root = true;
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("not-a-real-agent".into()),
                list: false,
                force: false,
                print: false,
                no_template: true,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: true,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown agent"),
            "gate must pass through to the agent-lookup path: {msg}"
        );
        assert!(
            !msg.contains("refuses to run as root"),
            "root gate must have been bypassed: {msg}"
        );
    }

    // ----- Sprint 6 S6-4: Template-registration section ------------------

    #[test]
    fn install_prints_template_registered_line_on_happy_path() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut buf: Vec<u8> = Vec::new();
        super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Template"),
            "missing 'Template' status line: {out}"
        );
        assert!(
            out.contains("invoke-claude-code-sam"),
            "missing derived template name: {out}"
        );
        assert!(
            out.contains("/mock/bin/agent"),
            "missing probed cmd path: {out}"
        );
        assert!(out.contains("run_as:"), "missing run_as segment: {out}");
        let forbidden = format!("{} aimx setup", "sudo");
        assert!(
            !out.contains(&forbidden),
            "legacy hint copy must be gone: {out}"
        );
    }

    #[test]
    fn install_prints_skipped_line_with_no_template() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut buf: Vec<u8> = Vec::new();
        super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: true,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Skipped template registration (--no-template)"),
            "missing skipped status line: {out}"
        );
        // Binary-not-found would prefix "Could not find"; since we
        // skipped the probe entirely, that text must not appear.
        assert!(!out.contains("Could not find"), "{out}");
    }

    #[test]
    fn install_prints_failed_line_when_binary_missing() {
        let tmp = TempDir::new().unwrap();
        let stub = TemplateStub {
            probe_result: None,
            ..Default::default()
        };
        let env = MockEnv::new(tmp.path().to_path_buf()).with_template_stub(stub);
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let exit = err.downcast_ref::<AgentSetupExitCode>().unwrap();
        assert_eq!(exit.code(), 3, "binary-not-found must map to exit 3");
        let msg = exit.message();
        assert!(
            msg.contains("Template registration failed:"),
            "missing failure line: {msg}"
        );
        assert!(
            msg.contains("Could not find 'claude' in $PATH"),
            "missing binary-missing copy: {msg}"
        );
        // The failure line must NOT also land on stdout — `run` prints it
        // to stderr via the `AgentSetupExitCode` handler. Duplicating
        // would mean operators see the same message twice.
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Template registration failed:"),
            "failure line must not duplicate onto stdout: {out}"
        );
    }

    #[test]
    fn install_prints_failed_line_when_daemon_missing() {
        let tmp = TempDir::new().unwrap();
        let stub = TemplateStub {
            submit_create_result: Err(TemplateCrudFallback::SocketMissing),
            ..Default::default()
        };
        let env = MockEnv::new(tmp.path().to_path_buf()).with_template_stub(stub);
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let exit = err.downcast_ref::<AgentSetupExitCode>().unwrap();
        assert_eq!(exit.code(), 2, "daemon-down must map to exit 2");
        let msg = exit.message();
        assert!(
            msg.contains("aimx serve is not running"),
            "missing daemon-down copy: {msg}"
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Template registration failed:"),
            "failure line must not duplicate onto stdout: {out}"
        );
    }

    #[test]
    fn install_hints_redetect_on_econflict() {
        let tmp = TempDir::new().unwrap();
        let stub = TemplateStub {
            submit_create_result: Err(TemplateCrudFallback::Daemon {
                code: "ECONFLICT".into(),
                reason: "template already exists".into(),
            }),
            ..Default::default()
        };
        let env = MockEnv::new(tmp.path().to_path_buf()).with_template_stub(stub);
        let mut buf: Vec<u8> = Vec::new();
        // ECONFLICT is a soft failure from the operator's perspective:
        // plugin files installed fine, they just need `--redetect`.
        super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("invoke-claude-code-sam"), "{out}");
        assert!(out.contains("already exists"), "{out}");
        assert!(out.contains("--redetect"), "{out}");
    }

    #[test]
    fn redetect_submits_template_update() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut buf: Vec<u8> = Vec::new();
        super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: true,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(env.template.submit_update_calls.borrow().len(), 1);
        assert_eq!(env.template.submit_create_calls.borrow().len(), 0);
        let update = env.template.submit_update_calls.borrow()[0].clone();
        assert_eq!(update.name, "invoke-claude-code-sam");
        assert_eq!(update.payload.cmd[0], "/mock/bin/agent");
        assert!(out.contains("re-pointed"), "{out}");
    }

    #[test]
    fn redetect_handles_enoent_cleanly() {
        let tmp = TempDir::new().unwrap();
        let stub = TemplateStub {
            submit_update_result: Err(TemplateCrudFallback::Daemon {
                code: "ENOENT".into(),
                reason: "template not found".into(),
            }),
            ..Default::default()
        };
        let env = MockEnv::new(tmp.path().to_path_buf()).with_template_stub(stub);
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: true,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let exit = err.downcast_ref::<AgentSetupExitCode>().unwrap();
        assert_eq!(exit.code(), 1);
        let msg = exit.message();
        assert!(msg.contains("does not exist yet"), "{msg}");
        assert!(
            msg.contains("without --redetect first"),
            "must nudge the operator to run without --redetect first: {msg}"
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Template registration failed:"),
            "failure line must not duplicate onto stdout: {out}"
        );
    }

    #[test]
    fn redetect_handles_eaccess_cleanly() {
        let tmp = TempDir::new().unwrap();
        let stub = TemplateStub {
            submit_update_result: Err(TemplateCrudFallback::Daemon {
                code: "EACCES".into(),
                reason: "not template owner".into(),
            }),
            ..Default::default()
        };
        let env = MockEnv::new(tmp.path().to_path_buf()).with_template_stub(stub);
        let mut buf: Vec<u8> = Vec::new();
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: false,
                redetect: true,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        let exit = err.downcast_ref::<AgentSetupExitCode>().unwrap();
        assert_eq!(exit.code(), 1);
        let msg = exit.message();
        assert!(
            msg.contains("Permission denied"),
            "must surface EACCES as 'Permission denied': {msg}"
        );
        assert!(
            msg.contains("invoke-claude-code-sam"),
            "must name the template: {msg}"
        );
        assert!(
            msg.contains("not template owner"),
            "must include the daemon-supplied reason: {msg}"
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Template registration failed:"),
            "failure line must not duplicate onto stdout: {out}"
        );
    }

    #[test]
    fn no_template_and_redetect_are_mutually_exclusive() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut buf: Vec<u8> = Vec::new();
        // Exercises the defense-in-depth runtime check — the clap-level
        // `conflicts_with = "redetect"` catches this earlier on the CLI
        // path, but the `pub fn` API bypasses clap and must still reject
        // the combination rather than silently picking a branch.
        let err = super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: false,
                no_template: true,
                redetect: true,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn print_mode_includes_rendered_template_toml() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());
        let mut buf: Vec<u8> = Vec::new();
        super::run_with_env_to_writer(
            super::RunOpts {
                agent: Some("claude-code".into()),
                list: false,
                force: false,
                print: true, // --print
                // `no_template` is unused under `--print` but left `false`
                // so the preview path still runs end-to-end.
                no_template: false,
                redetect: false,
                no_interactive: true,
                dangerously_allow_root: false,
                data_dir: None,
            },
            &env,
            &mut buf,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("=== template ==="), "{out}");
        // Rendered payload must reference the derived name + resolved
        // path + run_as. Enough keys to guarantee we're seeing real TOML
        // rather than, say, a copy-pasted placeholder.
        assert!(out.contains("name = \"invoke-claude-code-sam\""), "{out}");
        assert!(out.contains("run_as = \"sam\""), "{out}");
        assert!(out.contains("cmd = "), "{out}");
        assert!(
            !out.contains("=== hook templates ==="),
            "legacy `=== hook templates ===` section must be gone: {out}"
        );
    }

    // Make sure the legacy `sudo` ... `aimx setup` copy cannot reappear
    // in `agent_setup.rs`. This is the grep AC in S6-4 translated to a
    // compile-time assertion. The forbidden literal is assembled from
    // runtime pieces so this test file itself does not contain the exact
    // byte sequence the rest of the module is forbidden from carrying.
    #[test]
    fn agent_setup_source_has_no_legacy_setup_hint_copy() {
        let src = include_str!("agent_setup.rs");
        let forbidden = format!("{} aimx setup", "sudo");
        let hits: Vec<&str> = src
            .lines()
            .filter(|l| l.contains(&forbidden))
            .filter(|l| {
                // Ignore the single line in this test that synthesizes
                // the forbidden token at runtime via format!.
                !l.contains("let forbidden = format!")
            })
            .collect();
        assert!(
            hits.is_empty(),
            "agent_setup.rs still carries the legacy hint copy on lines: {hits:?}"
        );
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
            no_template: true,
            redetect: false,
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
            no_template: true,
            redetect: false,
        };
        let mut buf: Vec<u8> = Vec::new();
        install_to_writer(spec, &opts, &env, &mut buf).unwrap();
        let printed = String::from_utf8(buf).unwrap();

        // Extract the activation section and confirm it parses as JSON.
        // Sprint 4 added a `=== hook templates ===` section after the
        // activation block, so stop at the next `=== ` marker to keep
        // the JSON body uncontaminated.
        let (_, after) = printed.split_once("=== activation ===\n").unwrap();
        let snippet = after
            .lines()
            .take_while(|l| !l.starts_with("=== "))
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
            no_template: true,
            redetect: false,
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
            no_template: true,
            redetect: false,
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
        assert!(text.contains("references/hooks.md"));
        assert!(text.contains("references/troubleshooting.md"));
        assert!(text.contains("/var/lib/aimx/README.md"));
    }

    /// S5-5: every bundled agent (progressive_disclosure or not) picks
    /// up the primer text, so the "Creating hooks" section must be
    /// visible from the primer alone. A missing section means the
    /// primer edit got lost or reverted.
    #[test]
    fn primer_contains_creating_hooks_section() {
        let primer = AGENTS_DIR
            .get_file("common/aimx-primer.md")
            .unwrap()
            .contents();
        let text = std::str::from_utf8(primer).unwrap();
        assert!(
            text.contains("## Creating hooks"),
            "primer must contain 'Creating hooks' section"
        );
        assert!(text.contains("hook_list_templates"), "{text}");
        assert!(text.contains("hook_create"), "{text}");
        assert!(text.contains("hook_list"), "{text}");
        assert!(text.contains("hook_delete"), "{text}");
        assert!(
            text.contains("origin = \"mcp\"") || text.contains("origin = \\\"mcp\\\""),
            "primer must explain the origin tag"
        );
    }

    /// S5-5: new reference file must be present in the embedded bundle
    /// and cover the four new tools end-to-end.
    #[test]
    fn hooks_reference_file_bundled_and_comprehensive() {
        let contents = AGENTS_DIR
            .get_file("common/references/hooks.md")
            .expect("references/hooks.md must be embedded")
            .contents();
        let text = std::str::from_utf8(contents).unwrap();
        // Every hook tool documented by name.
        assert!(text.contains("hook_list_templates"), "{text}");
        assert!(text.contains("hook_create"), "{text}");
        assert!(text.contains("hook_list"), "{text}");
        assert!(text.contains("hook_delete"), "{text}");
        // Safety / origin model present.
        assert!(text.contains("template"), "{text}");
        assert!(text.contains("origin"), "{text}");
        assert!(
            text.contains("ERR origin-protected") || text.contains("origin-protected"),
            "must mention origin-protected"
        );
        // Troubleshooting subsection.
        assert!(text.contains("Troubleshooting"), "{text}");
    }

    /// S5-5: progressive-disclosure bundles (Claude Code, Codex,
    /// OpenClaw, Hermes) copy every references/*.md, so the new
    /// `hooks.md` lands alongside the existing four.
    #[test]
    fn progressive_disclosure_bundles_include_hooks_reference() {
        let tmp = TempDir::new().unwrap();
        let env = MockEnv::new(tmp.path().to_path_buf());

        run_with_env(Some("claude-code".into()), false, false, false, None, &env).unwrap();

        let refs = tmp
            .path()
            .join(".claude/plugins/aimx/skills/aimx/references");
        assert!(
            refs.join("hooks.md").exists(),
            "claude-code bundle must include references/hooks.md"
        );
        let installed = std::fs::read_to_string(refs.join("hooks.md")).unwrap();
        let embedded = AGENTS_DIR
            .get_file("common/references/hooks.md")
            .unwrap()
            .contents();
        assert_eq!(installed.as_bytes(), embedded);
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

    // ----- Sprint 6 S6-1: $PATH probe --------------------------------------

    /// Shell `$PATH`-style OsString from a slice of paths. Helper so tests
    /// don't embed `:` separators by hand.
    fn pathsep(entries: &[&Path]) -> OsString {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes: Vec<u8> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                bytes.push(b':');
            }
            bytes.extend_from_slice(e.as_os_str().as_bytes());
        }
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes)
    }

    #[cfg(unix)]
    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    fn write_non_executable(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"not-executable").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_finds_binary_in_first_entry() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        let expected = write_executable(&a, "claude");
        let path = pathsep(&[&a]);

        let got = probe_path_in("claude", &path).unwrap();
        assert_eq!(got, std::fs::canonicalize(&expected).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_first_match_wins_across_multiple_entries() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let first = write_executable(&a, "claude");
        let _second = write_executable(&b, "claude");
        let path = pathsep(&[&a, &b]);

        let got = probe_path_in("claude", &path).unwrap();
        assert_eq!(got, std::fs::canonicalize(&first).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_finds_binary_in_later_entry_when_earlier_empty() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let second = write_executable(&b, "claude");
        let path = pathsep(&[&a, &b]);

        let got = probe_path_in("claude", &path).unwrap();
        assert_eq!(got, std::fs::canonicalize(&second).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_returns_none_when_binary_absent() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        let path = pathsep(&[&a]);
        assert!(probe_path_in("claude", &path).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_skips_non_executable_file() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // Plain-perms file in `a`, real executable in `b`. The probe
        // must skip `a/claude` and pick up `b/claude`.
        write_non_executable(&a, "claude");
        let exec = write_executable(&b, "claude");
        let path = pathsep(&[&a, &b]);

        let got = probe_path_in("claude", &path).unwrap();
        assert_eq!(got, std::fs::canonicalize(&exec).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_skips_non_directory_path_entry() {
        let tmp = TempDir::new().unwrap();
        // A plain file posing as a `$PATH` entry: `stat` on `<file>/claude`
        // must fail cleanly and the probe should move on.
        let file_entry = tmp.path().join("not-a-dir");
        std::fs::write(&file_entry, b"hi").unwrap();
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&b).unwrap();
        let exec = write_executable(&b, "claude");
        let path = pathsep(&[&file_entry, &b]);

        let got = probe_path_in("claude", &path).unwrap();
        assert_eq!(got, std::fs::canonicalize(&exec).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_returns_none_on_empty_path_var() {
        let empty = OsString::from("");
        assert!(probe_path_in("claude", &empty).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn probe_path_real_env_reads_var_os() {
        // Drive the public helper against a synthetic `$PATH`. Uses
        // `env::set_var` under `serial_test::serial` so other tests
        // don't observe the mutation.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        let expected = write_executable(&a, "claude");
        let path = pathsep(&[&a]);

        let prev = std::env::var_os("PATH");
        // SAFETY: single-threaded scope inside this test (serialized by
        // serial_test). We restore the previous value before returning.
        unsafe {
            std::env::set_var("PATH", &path);
        }
        let got = probe_path("claude");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        assert_eq!(got.unwrap(), std::fs::canonicalize(&expected).unwrap());
    }

    #[test]
    fn every_registered_agent_has_canonical_binary() {
        // Registry lock-in: accidentally setting `canonical_binary` to
        // the empty string would silently disable the probe. Every
        // registered agent must declare a non-empty probe name.
        for spec in registry() {
            assert!(
                !spec.canonical_binary.is_empty(),
                "agent '{}' has an empty canonical_binary",
                spec.name
            );
        }
    }

    #[test]
    fn canonical_binary_maps_per_prd_section_six_six() {
        // Spot check the explicit mappings per Sprint 6 S6-1:
        // claude-code → claude, codex → codex, opencode → opencode,
        // gemini-cli / gemini → gemini, goose → goose, openclaw → openclaw.
        let expect: &[(&str, &str)] = &[
            ("claude-code", "claude"),
            ("codex", "codex"),
            ("opencode", "opencode"),
            ("gemini", "gemini"),
            ("goose", "goose"),
            ("openclaw", "openclaw"),
        ];
        for (agent, want_bin) in expect {
            let spec = find_agent(agent).expect("registered");
            assert_eq!(
                spec.canonical_binary, *want_bin,
                "canonical_binary mismatch for agent '{agent}'"
            );
        }
    }
}
