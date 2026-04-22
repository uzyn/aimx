//! Per-user inverse of `aimx agent-setup`.
//!
//! `aimx agent-cleanup <agent>` drops the
//! `invoke-<agent>-<caller_username>` template that the matching
//! `agent-setup` registered over UDS. With `--full` it also removes
//! the plugin files under `$HOME` that `agent-setup` laid down.
//!
//! The command runs per-user and refuses root. Daemon-down with
//! `--full` still wipes plugin files and exits `2`, pointing the
//! operator at `sudo aimx hooks prune --orphans` to clean up the
//! template side after the daemon restarts. (PRD §6.11.)

use crate::agent_setup::{
    AgentEnv, AgentSpec, RealAgentEnv, derive_template_name, find_agent, resolve_dest,
};
use crate::hook_client::{TemplateCrudFallback, submit_template_delete_via_daemon};
use crate::send_protocol::TemplateDeleteRequest;
use crate::term;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Exit code emitted when the daemon is unreachable and `--full` had to
/// fall back to plugin-file removal only. Mirrors `agent-setup`'s
/// socket-missing exit code so operators can script both commands
/// against the same non-zero return.
const EXIT_DAEMON_UNREACHABLE: i32 = 2;

/// CLI options for one `aimx agent-cleanup` invocation.
pub struct RunOpts {
    pub agent: String,
    pub full: bool,
    pub yes: bool,
}

/// Internal outcome tag used for tests. Kept crate-private — the
/// public surface is the exit code + writer output from `run`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// Template and (with `--full`) plugin files removed cleanly.
    Ok,
    /// Daemon was unreachable; plugin files were handled (or skipped).
    DaemonUnreachable,
}

/// Entry point called from `main.rs`.
pub fn run(opts: RunOpts) -> Result<(), Box<dyn std::error::Error>> {
    let env = RealAgentEnv;
    let outcome = run_with_env(opts, &env, &mut io::stdout())?;
    match outcome {
        CleanupOutcome::Ok => Ok(()),
        CleanupOutcome::DaemonUnreachable => std::process::exit(EXIT_DAEMON_UNREACHABLE),
    }
}

/// Testable core of [`run`]. Writes human-facing output to `out`.
pub(crate) fn run_with_env(
    opts: RunOpts,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<CleanupOutcome, Box<dyn std::error::Error>> {
    if env.is_root() {
        return Err("agent-cleanup is a per-user operation. Run without sudo or as root".into());
    }

    let spec = find_agent(&opts.agent).ok_or_else(|| {
        format!(
            "unknown agent '{}'; run `aimx agent-setup --list` to see supported agents",
            opts.agent
        )
    })?;

    let username = env
        .caller_username()
        .ok_or_else(|| "could not resolve caller username via getpwuid".to_string())?;

    let template_name = derive_template_name(spec.name, &username).map_err(|e| e.to_string())?;

    // 1. Ask the daemon to drop the template. The daemon's
    //    `TEMPLATE-DELETE` handler rejects any caller whose uid does
    //    not match the template's `run_as`, so this is the only
    //    pairing that actually succeeds in practice.
    let request = TemplateDeleteRequest {
        name: template_name.clone(),
    };
    let template_result = submit_template_delete_via_daemon(&request);

    let mut daemon_unreachable = false;

    match template_result {
        Ok(()) => {
            writeln!(
                out,
                "{} {} removed",
                term::success("Template"),
                term::highlight(&template_name),
            )?;
        }
        Err(TemplateCrudFallback::SocketMissing) => {
            daemon_unreachable = true;
            writeln!(
                out,
                "{} aimx serve is not running; could not remove template over the socket.",
                term::warn("Warning:"),
            )?;
        }
        Err(TemplateCrudFallback::Daemon { code, reason }) => {
            // `NOTFOUND` is a benign no-op (e.g. the template was never
            // registered on this box). Other daemon errors surface as-is.
            if code == "NOTFOUND" {
                writeln!(
                    out,
                    "{} {} was not registered (nothing to do).",
                    term::info("Template"),
                    term::highlight(&template_name),
                )?;
            } else {
                writeln!(
                    out,
                    "{} failed to remove template {}: [{code}] {reason}",
                    term::error("Error:"),
                    term::highlight(&template_name),
                )?;
                return Err(format!("template delete rejected: [{code}] {reason}").into());
            }
        }
        Err(TemplateCrudFallback::Local(msg)) => {
            return Err(msg.into());
        }
    }

    // 2. With `--full`, also remove the plugin files that `agent-setup`
    //    laid down under the caller's `$HOME`. This runs regardless of
    //    whether the daemon answered — the operator is uninstalling the
    //    agent and has already consented to destructive removal.
    if opts.full {
        remove_plugin_files(spec, env, opts.yes, out)?;
    }

    if daemon_unreachable {
        writeln!(
            out,
            "{}",
            term::info(
                "daemon unreachable; run `sudo aimx hooks prune --orphans` after restarting to clean up templates."
            )
        )?;
        return Ok(CleanupOutcome::DaemonUnreachable);
    }

    Ok(CleanupOutcome::Ok)
}

/// Remove plugin files previously laid down by `agent-setup`.
///
/// `spec.dest_template` encodes the destination shape. Most agents
/// install a directory tree (e.g. `~/.claude/plugins/aimx/`), so we
/// remove the whole directory. Goose is the one exception — it
/// installs a single `aimx.yaml` file under the recipes directory;
/// for that shape we remove only the file to avoid collateral damage
/// to unrelated recipes the user might have added.
fn remove_plugin_files(
    spec: &AgentSpec,
    env: &dyn AgentEnv,
    yes: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let dest_root = resolve_dest(spec.dest_template, env)?;
    let (target_desc, removal_target) = plugin_removal_target(spec, &dest_root);

    if !removal_target.exists() {
        writeln!(
            out,
            "{} {} (already absent)",
            term::info("Plugin:"),
            term::highlight(&target_desc),
        )?;
        return Ok(());
    }

    if !yes && env.is_stdin_tty() {
        write!(out, "Remove plugin files at {}? [y/N] ", target_desc)?;
        out.flush().ok();
        let line = env.read_line()?;
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            writeln!(out, "{}", term::info("Plugin removal skipped."))?;
            return Ok(());
        }
    }

    if removal_target.is_dir() {
        std::fs::remove_dir_all(&removal_target)?;
    } else {
        std::fs::remove_file(&removal_target)?;
    }

    writeln!(
        out,
        "{} {}",
        term::success("Removed"),
        term::highlight(&target_desc),
    )?;
    Ok(())
}

/// Select the path that `--full` should delete, plus a human-readable
/// description for prompts and log lines.
///
/// The Goose spec writes a single `aimx.yaml` under
/// `~/.config/goose/recipes/`; the others write a directory tree
/// (e.g. `~/.claude/plugins/aimx/`). Crate-private so tests can pin
/// the asymmetric behavior.
pub(crate) fn plugin_removal_target(spec: &AgentSpec, dest_root: &Path) -> (String, PathBuf) {
    if spec.name == "goose" {
        let file = dest_root.join("aimx.yaml");
        (file.display().to_string(), file)
    } else {
        (dest_root.display().to_string(), dest_root.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_setup::AgentEnv;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct TestEnv {
        home: PathBuf,
        username: String,
        is_root: bool,
        is_tty: bool,
        scripted_input: RefCell<Vec<String>>,
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
            self.is_tty
        }
        fn read_line(&self) -> std::io::Result<String> {
            self.scripted_input
                .borrow_mut()
                .pop()
                .ok_or_else(|| std::io::Error::other("no scripted input"))
        }
        fn caller_username(&self) -> Option<String> {
            Some(self.username.clone())
        }
        fn submit_template_create(
            &self,
            _request: &crate::send_protocol::TemplateCreateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local(
                "not used in cleanup tests".into(),
            ))
        }
        fn submit_template_update(
            &self,
            _request: &crate::send_protocol::TemplateUpdateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local(
                "not used in cleanup tests".into(),
            ))
        }
    }

    impl TestEnv {
        fn new(home: PathBuf, username: &str) -> Self {
            Self {
                home,
                username: username.to_string(),
                is_root: false,
                is_tty: false,
                scripted_input: RefCell::new(Vec::new()),
            }
        }
    }

    #[test]
    fn refuses_to_run_as_root() {
        let tmp = TempDir::new().unwrap();
        let mut env = TestEnv::new(tmp.path().to_path_buf(), "alice");
        env.is_root = true;

        let opts = RunOpts {
            agent: "claude-code".to_string(),
            full: false,
            yes: false,
        };
        let mut out = Vec::new();
        let err = run_with_env(opts, &env, &mut out).unwrap_err();
        assert!(err.to_string().contains("per-user operation"));
    }

    #[test]
    fn rejects_unknown_agent() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice");

        let opts = RunOpts {
            agent: "nonesuch".to_string(),
            full: false,
            yes: false,
        };
        let mut out = Vec::new();
        let err = run_with_env(opts, &env, &mut out).unwrap_err();
        assert!(err.to_string().contains("unknown agent"));
    }

    #[test]
    fn plugin_removal_target_for_directory_agent() {
        let spec = find_agent("claude-code").unwrap();
        let root = PathBuf::from("/home/alice/.claude/plugins/aimx");
        let (desc, target) = plugin_removal_target(spec, &root);
        assert_eq!(target, root);
        assert!(desc.ends_with("/.claude/plugins/aimx"));
    }

    #[test]
    fn plugin_removal_target_for_goose_uses_single_file() {
        let spec = find_agent("goose").unwrap();
        let root = PathBuf::from("/home/alice/.config/goose/recipes");
        let (desc, target) = plugin_removal_target(spec, &root);
        assert_eq!(target, root.join("aimx.yaml"));
        assert!(desc.ends_with("/aimx.yaml"));
    }
}
