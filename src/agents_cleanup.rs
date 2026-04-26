//! Internal cleanup core shared by `aimx agents remove`.
//!
//! Removes the plugin files under `$HOME` that the installer laid down.
//!
//! Refuses to run as root.

use crate::agents_setup::{AgentEnv, AgentSpec, find_agent, resolve_dest};
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
    /// Plugin files removed cleanly (or were already absent).
    Ok,
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

    if opts.full {
        remove_plugin_files(spec, env, opts.yes, out)?;
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

    #[test]
    fn full_removes_directory() {
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
            full: true,
            yes: true,
        };
        let mut out = Vec::new();
        let outcome = run_with_env(opts, &env, &mut out).expect("remove must succeed");
        assert_eq!(outcome, CleanupOutcome::Ok);
        assert!(!dest.exists(), "dest must be removed");
    }
}
