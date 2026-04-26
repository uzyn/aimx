//! Internal cleanup core shared by `aimx agents remove`.
//!
//! Drops the `invoke-<agent>-<caller_username>` template that
//! `aimx agents setup` registered over UDS. With `--full` it also
//! removes the plugin files under `$HOME` that the installer laid down.
//!
//! Refuses to run as root. Daemon-down with `--full` still wipes
//! plugin files and reports `DaemonUnreachable` so the caller can
//! exit non-zero, pointing the operator at
//! `sudo aimx hooks prune --orphans` to clean up the template side
//! after the daemon restarts.

use crate::agents_setup::TemplateDeleteRequest;
use crate::agents_setup::{AgentEnv, AgentSpec, derive_template_name, find_agent, resolve_dest};
use crate::hook_client::TemplateCrudFallback;
use crate::term;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Internal options for one cleanup pass.
pub struct RunOpts {
    pub agent: String,
    pub full: bool,
    pub yes: bool,
}

/// Internal outcome tag used for tests. Kept crate-private — the
/// public surface is the exit code + writer output from `agents_remove::run`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// Template and (with `--full`) plugin files removed cleanly.
    Ok,
    /// Daemon was unreachable; plugin files were handled (or skipped).
    DaemonUnreachable,
}

/// Testable core of [`run`]. Writes human-facing output to `out`.
pub(crate) fn run_with_env(
    opts: RunOpts,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<CleanupOutcome, Box<dyn std::error::Error>> {
    if env.is_root() {
        return Err("agents remove is a per-user operation. Run without sudo or as root".into());
    }

    let spec = find_agent(&opts.agent).ok_or_else(|| {
        format!(
            "unknown agent '{}'; run `aimx agents list` to see supported agents",
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
    let template_result = env.submit_template_delete(&request);

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
            writeln!(
                out,
                "{} failed to remove template {}: {msg}",
                term::error("Error:"),
                term::highlight(&template_name),
            )?;
            return Err(msg.into());
        }
    }

    // 2. With `--full`, also remove the plugin files that `agents setup`
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

/// Remove plugin files previously laid down by `agents setup`.
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
    use crate::agents_setup::AgentEnv;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct TestEnv {
        home: PathBuf,
        username: String,
        is_root: bool,
        is_tty: bool,
        scripted_input: RefCell<Vec<String>>,
        /// Canned result for `submit_template_delete`. Default is `Ok(())`
        /// so tests that don't care about the daemon path stay concise.
        template_delete_result: RefCell<Result<(), TemplateCrudFallback>>,
        /// Captured delete-request names so tests can assert we asked for
        /// the right template without peeking into private fields.
        delete_calls: RefCell<Vec<String>>,
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
            _request: &crate::agents_setup::TemplateCreateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local(
                "not used in cleanup tests".into(),
            ))
        }
        fn submit_template_update(
            &self,
            _request: &crate::agents_setup::TemplateUpdateRequest,
        ) -> Result<(), TemplateCrudFallback> {
            Err(TemplateCrudFallback::Local(
                "not used in cleanup tests".into(),
            ))
        }
        fn submit_template_delete(
            &self,
            request: &TemplateDeleteRequest,
        ) -> Result<(), TemplateCrudFallback> {
            self.delete_calls.borrow_mut().push(request.name.clone());
            match &*self.template_delete_result.borrow() {
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

    impl TestEnv {
        fn new(home: PathBuf, username: &str) -> Self {
            Self {
                home,
                username: username.to_string(),
                is_root: false,
                is_tty: false,
                scripted_input: RefCell::new(Vec::new()),
                template_delete_result: RefCell::new(Ok(())),
                delete_calls: RefCell::new(Vec::new()),
            }
        }

        fn with_delete_result(self, result: Result<(), TemplateCrudFallback>) -> Self {
            *self.template_delete_result.borrow_mut() = result;
            self
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

    /// Review NB-3: daemon socket missing must map to
    /// `CleanupOutcome::DaemonUnreachable` and surface a Warning banner,
    /// not a hard error. Plugin files are not touched without `--full`.
    #[test]
    fn socket_missing_maps_to_daemon_unreachable_outcome() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice")
            .with_delete_result(Err(TemplateCrudFallback::SocketMissing));

        let opts = RunOpts {
            agent: "claude-code".to_string(),
            full: false,
            yes: false,
        };
        let mut out = Vec::new();
        let outcome = run_with_env(opts, &env, &mut out).expect("should not be a hard error");
        assert_eq!(outcome, CleanupOutcome::DaemonUnreachable);

        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains("aimx serve is not running"),
            "expected socket-missing warning, got: {stdout}"
        );
        assert!(
            stdout.contains("prune --orphans"),
            "expected recovery hint, got: {stdout}"
        );

        // And the right template was asked about.
        assert_eq!(
            env.delete_calls.borrow().as_slice(),
            &["invoke-claude-code-alice".to_string()]
        );
    }

    /// Review NB-3: daemon returning `NOTFOUND` for a template that was
    /// never registered on this box is a benign no-op — the command
    /// succeeds and the user sees an informational "nothing to do" line,
    /// not an error.
    #[test]
    fn daemon_notfound_is_benign_no_op() {
        let tmp = TempDir::new().unwrap();
        let env = TestEnv::new(tmp.path().to_path_buf(), "alice").with_delete_result(Err(
            TemplateCrudFallback::Daemon {
                code: "NOTFOUND".to_string(),
                reason: "template not found".to_string(),
            },
        ));

        let opts = RunOpts {
            agent: "claude-code".to_string(),
            full: false,
            yes: false,
        };
        let mut out = Vec::new();
        let outcome = run_with_env(opts, &env, &mut out).expect("NOTFOUND must not be an error");
        assert_eq!(outcome, CleanupOutcome::Ok);

        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains("not registered"),
            "expected 'not registered' info line, got: {stdout}"
        );
        assert!(
            stdout.contains("invoke-claude-code-alice"),
            "expected template name in output, got: {stdout}"
        );
    }
}
