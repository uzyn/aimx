//! `aimx agents remove <agent>`: per-user inverse of `aimx agents setup`.
//!
//! Removes the plugin files under `$HOME` that `agents setup` laid
//! down, drops the matching `invoke-<agent>-<username>` template over
//! the daemon UDS, and prints a per-agent cleanup hint pointing at any
//! external command the operator still needs to run (for example
//! `claude mcp remove aimx`).
//!
//! Refuses to run as root by default. The same `--dangerously-allow-root`
//! escape hatch as `aimx agents setup` exists for single-user
//! root-login VPS setups; on any machine with a regular user, prefer
//! `sudo -u <user> aimx agents remove <agent>`.

use crate::agent_cleanup::{self, plugin_removal_target};
use crate::agent_setup::{
    AgentEnv, AgentSpec, ROOT_REFUSAL_MESSAGE, RealAgentEnv, find_agent, home_dir_for_user,
    resolve_dest,
};
use std::io::{self, Write};
use std::path::PathBuf;

/// Exit code emitted when the daemon was unreachable. Mirrors
/// `aimx agents setup` / `aimx agent-cleanup`'s socket-missing exit
/// code so operators can script both commands against the same non-zero
/// return.
const EXIT_DAEMON_UNREACHABLE: i32 = 2;

/// CLI options for one `aimx agents remove` invocation.
pub struct RunOpts {
    pub agent: String,
    pub dangerously_allow_root: bool,
}

/// Entry point called from `main.rs`.
pub fn run(opts: RunOpts) -> Result<(), Box<dyn std::error::Error>> {
    let env = RealAgentEnv;
    let outcome = run_with_env(opts, &env, &mut io::stdout())?;
    match outcome {
        agent_cleanup::CleanupOutcome::Ok => Ok(()),
        agent_cleanup::CleanupOutcome::DaemonUnreachable => {
            std::process::exit(EXIT_DAEMON_UNREACHABLE)
        }
    }
}

/// Testable core of [`run`]. Removes plugin files unconditionally
/// (no per-agent file-vs-directory prompt), submits the
/// `TEMPLATE-DELETE` over UDS via [`agent_cleanup::run_with_env`], then
/// prints the per-agent cleanup hint via [`removal_hint`].
pub(crate) fn run_with_env(
    opts: RunOpts,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<agent_cleanup::CleanupOutcome, Box<dyn std::error::Error>> {
    if env.is_root() && !opts.dangerously_allow_root {
        return Err(ROOT_REFUSAL_MESSAGE.into());
    }

    let spec = find_agent(&opts.agent).ok_or_else(|| {
        format!(
            "Unknown agent '{}'. Run `aimx agents list` to see supported agents.",
            opts.agent
        )
    })?;

    // Apply the same `--dangerously-allow-root` home override as
    // `agent_setup::run_with_env_to_writer` so the resolved dest path
    // points at /root's home, not the ambient $HOME.
    let result = if env.is_root() && opts.dangerously_allow_root {
        let root_home = home_dir_for_user("root").unwrap_or_else(|| PathBuf::from("/root"));
        let override_env = crate::agent_setup::OverrideHomeEnv::new(env, root_home);
        do_remove(spec, opts.dangerously_allow_root, &override_env, out)?
    } else {
        do_remove(spec, opts.dangerously_allow_root, env, out)?
    };

    Ok(result)
}

fn do_remove(
    spec: &AgentSpec,
    dangerously_allow_root: bool,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<agent_cleanup::CleanupOutcome, Box<dyn std::error::Error>> {
    // Resolve the removal target up front so we can report whether the
    // wiring actually existed before the daemon call.
    let dest_root = resolve_dest(spec.dest_template, env)?;
    let (target_desc, removal_target) = plugin_removal_target(spec, &dest_root);
    let pre_existed = removal_target.exists();

    // Delegate to the existing cleanup core in `--full --yes` mode.
    // It removes plugin files and submits TEMPLATE-DELETE in one
    // pass, with the same warning + non-zero exit on daemon-down.
    let cleanup_opts = agent_cleanup::RunOpts {
        agent: spec.name.to_string(),
        full: true,
        yes: true,
    };
    // The underlying cleanup core re-runs the root gate. With
    // `--dangerously-allow-root` set, env.is_root() is still true,
    // which would cause the per-user refusal there. Skip that
    // refusal by passing through a wrapper env that masks is_root.
    let outcome = if dangerously_allow_root {
        let masked = MaskRootEnv { inner: env };
        agent_cleanup::run_with_env(cleanup_opts, &masked, out)?
    } else {
        agent_cleanup::run_with_env(cleanup_opts, env, out)?
    };

    if !pre_existed {
        writeln!(
            out,
            "{} no plugin files were present at {} (already removed?)",
            crate::term::warn_mark(),
            crate::term::highlight(&target_desc),
        )?;
    }

    writeln!(out)?;
    writeln!(out, "{}", removal_hint(spec))?;

    Ok(outcome)
}

/// Per-agent cleanup hint. Emitted at the end of
/// [`run_with_env`] so the operator knows which (if any) external
/// command they still need to run after `aimx` has wiped its own
/// footprint. Returned as a plain `String` so callers can print it
/// alongside whatever banner / mark they prefer.
pub fn removal_hint(spec: &AgentSpec) -> String {
    match spec.name {
        "claude-code" => "Run `claude mcp remove aimx` to also unregister the MCP server from Claude Code.".to_string(),
        "codex" => "Run `codex mcp remove aimx` to also unregister the MCP server from Codex CLI.".to_string(),
        "opencode" => {
            "Remove the `aimx` block from `~/.config/opencode/opencode.json` to also unregister the MCP server.".to_string()
        }
        "gemini" => {
            "Remove the `aimx` entry from `~/.gemini/settings.json` to also unregister the MCP server.".to_string()
        }
        "goose" => {
            "(Goose discovers recipes by filename; no further action needed.)".to_string()
        }
        "openclaw" => {
            "Run `openclaw mcp remove aimx` if your version supports it; otherwise edit the OpenClaw MCP config by hand.".to_string()
        }
        "hermes" => {
            "Remove the `aimx` block from `~/.hermes/config.yaml` under `mcp_servers:` to also unregister the MCP server.".to_string()
        }
        // Future-proof: never panic on a registry-name mismatch.
        other => format!("(No agent-specific cleanup hint registered for `{other}`.)"),
    }
}

/// Wrapper that hides the underlying env's root status so the
/// `agent_cleanup` core's per-user refusal doesn't trip when we've
/// already accepted `--dangerously-allow-root` here.
struct MaskRootEnv<'a> {
    inner: &'a dyn AgentEnv,
}

impl<'a> AgentEnv for MaskRootEnv<'a> {
    fn home_dir(&self) -> Option<PathBuf> {
        self.inner.home_dir()
    }
    fn xdg_config_home(&self) -> Option<PathBuf> {
        self.inner.xdg_config_home()
    }
    fn is_root(&self) -> bool {
        false
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
    fn submit_template_create(
        &self,
        request: &crate::send_protocol::TemplateCreateRequest,
    ) -> Result<(), crate::hook_client::TemplateCrudFallback> {
        self.inner.submit_template_create(request)
    }
    fn submit_template_update(
        &self,
        request: &crate::send_protocol::TemplateUpdateRequest,
    ) -> Result<(), crate::hook_client::TemplateCrudFallback> {
        self.inner.submit_template_update(request)
    }
    fn submit_template_delete(
        &self,
        request: &crate::send_protocol::TemplateDeleteRequest,
    ) -> Result<(), crate::hook_client::TemplateCrudFallback> {
        self.inner.submit_template_delete(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_client::TemplateCrudFallback;
    use crate::send_protocol::{
        TemplateCreateRequest, TemplateDeleteRequest, TemplateUpdateRequest,
    };
    use std::cell::RefCell;
    use tempfile::TempDir;

    struct TestEnv {
        home: PathBuf,
        username: String,
        is_root: bool,
        delete_calls: RefCell<Vec<String>>,
        delete_result: RefCell<Result<(), TemplateCrudFallback>>,
    }

    impl TestEnv {
        fn new(home: PathBuf, username: &str) -> Self {
            Self {
                home,
                username: username.to_string(),
                is_root: false,
                delete_calls: RefCell::new(Vec::new()),
                delete_result: RefCell::new(Ok(())),
            }
        }
    }

    impl AgentEnv for TestEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn xdg_config_home(&self) -> Option<PathBuf> {
            None
        }
        fn is_root(&self) -> bool {
            self.is_root
        }
        fn is_stdin_tty(&self) -> bool {
            false
        }
        fn read_line(&self) -> io::Result<String> {
            Err(io::Error::other("not used in remove tests"))
        }
        fn caller_username(&self) -> Option<String> {
            Some(self.username.clone())
        }
        fn submit_template_create(
            &self,
            _request: &TemplateCreateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local("not used".into()))
        }
        fn submit_template_update(
            &self,
            _request: &TemplateUpdateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local("not used".into()))
        }
        fn submit_template_delete(
            &self,
            request: &TemplateDeleteRequest,
        ) -> Result<(), TemplateCrudFallback> {
            self.delete_calls.borrow_mut().push(request.name.clone());
            match &*self.delete_result.borrow() {
                Ok(()) => Ok(()),
                Err(TemplateCrudFallback::SocketMissing) => {
                    Err(TemplateCrudFallback::SocketMissing)
                }
                Err(TemplateCrudFallback::Daemon { code, reason }) => {
                    Err(TemplateCrudFallback::Daemon {
                        code: code.clone(),
                        reason: reason.clone(),
                    })
                }
                Err(TemplateCrudFallback::Local(msg)) => {
                    Err(TemplateCrudFallback::Local(msg.clone()))
                }
            }
        }
    }

    #[test]
    fn agents_remove_filesystem_cleanup() {
        // Pre-populate the dest dir; after running, dest must not exist.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join(".claude").join("plugins").join("aimx");
        std::fs::create_dir_all(dest.join(".claude-plugin")).unwrap();
        std::fs::write(
            dest.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"aimx"}"#,
        )
        .unwrap();
        assert!(dest.exists());

        let env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        let opts = RunOpts {
            agent: "claude-code".to_string(),
            dangerously_allow_root: false,
        };
        let mut out = Vec::new();
        let outcome = run_with_env(opts, &env, &mut out).expect("remove must succeed");
        assert!(matches!(outcome, agent_cleanup::CleanupOutcome::Ok));
        assert!(!dest.exists(), "dest must be removed");
    }

    #[test]
    fn agents_remove_unregisters_template() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        let opts = RunOpts {
            agent: "claude-code".to_string(),
            dangerously_allow_root: false,
        };
        let mut out = Vec::new();
        run_with_env(opts, &env, &mut out).expect("remove must succeed");
        assert_eq!(
            env.delete_calls.borrow().as_slice(),
            &["invoke-claude-code-alice".to_string()],
            "remove must submit TEMPLATE-DELETE for the derived name"
        );
    }

    #[test]
    fn agents_remove_prints_agent_specific_hint() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        let opts = RunOpts {
            agent: "claude-code".to_string(),
            dangerously_allow_root: false,
        };
        let mut out = Vec::new();
        run_with_env(opts, &env, &mut out).expect("remove must succeed");
        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains("claude mcp remove aimx"),
            "expected claude-code cleanup hint, got: {stdout}"
        );
    }

    #[test]
    fn agents_remove_unknown_agent_errors_clearly() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        let opts = RunOpts {
            agent: "nonesuch".to_string(),
            dangerously_allow_root: false,
        };
        let mut out = Vec::new();
        let err = run_with_env(opts, &env, &mut out).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown agent"),
            "expected 'Unknown agent' in error, got: {msg}"
        );
        assert!(
            msg.contains("nonesuch"),
            "expected the bad agent name in the error, got: {msg}"
        );
    }

    #[test]
    fn agents_remove_refuses_root() {
        let tmp = TempDir::new().unwrap();
        let mut env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        env.is_root = true;
        let opts = RunOpts {
            agent: "claude-code".to_string(),
            dangerously_allow_root: false,
        };
        let mut out = Vec::new();
        let err = run_with_env(opts, &env, &mut out).unwrap_err();
        assert!(
            err.to_string().contains("refuses to run as root"),
            "expected root-refusal, got: {err}"
        );
    }

    #[test]
    fn removal_hint_covers_every_registered_agent() {
        // Every registered agent must have an explicit hint (no
        // `(No agent-specific cleanup hint ...)` fallback for known
        // agents). Walk the registry; the fallback path uses
        // parentheses + the literal "No agent-specific" prefix.
        for spec in crate::agent_setup::registry() {
            let hint = removal_hint(spec);
            assert!(
                !hint.starts_with("(No agent-specific"),
                "agent '{}' must have a removal hint",
                spec.name
            );
            assert!(
                !hint.is_empty(),
                "agent '{}' removal hint must not be empty",
                spec.name
            );
        }
    }
}
