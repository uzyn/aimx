//! Auto-registers the aimx MCP server with agent CLIs that expose one
//! (`claude mcp add`, `codex mcp add`). Called from `agents_setup` after
//! the skill files are written.
//!
//! For each supported agent we (1) best-effort remove any existing
//! `aimx` entry — ignoring its exit code so a missing entry doesn't
//! surface as an error — then (2) add the entry pointing at
//! `/usr/local/bin/aimx [--data-dir <path>] mcp`. If the agent CLI
//! isn't on PATH the installer falls back to printing the manual
//! command via the per-agent activation hint.
//!
//! `McpCli` exists so tests can assert exact argv without spawning
//! processes; `RealMcpCli` is the production thin wrapper around
//! `std::process::Command::output()`.

use std::path::Path;
use std::process::{Command, Output};

const AIMX_BINARY: &str = "/usr/local/bin/aimx";

#[derive(Debug)]
pub enum McpRegistration {
    /// `mcp add` returned exit 0.
    Registered,
    /// The agent's CLI is not on PATH (`io::ErrorKind::NotFound`).
    CliMissing,
    /// `mcp add` ran but exited non-zero, or the spawn itself failed
    /// for a reason other than a missing binary. `stderr` is captured
    /// so the caller can surface diagnostics; `exit_code` is `Some(n)`
    /// when the child exited with a real status code, and `None` when
    /// the child was signal-killed or never spawned (`io::Error` path).
    RegisterFailed {
        stderr: String,
        exit_code: Option<i32>,
    },
}

/// Trait so tests can substitute a mock without spawning child
/// processes. Production callers use `RealMcpCli`.
pub trait McpCli {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output>;
}

pub struct RealMcpCli;

impl McpCli for RealMcpCli {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output> {
        Command::new(program).args(args).output()
    }
}

/// Used by `AgentEnv` test mocks so `cargo test` can drive
/// `install_to_writer` for `claude-code` / `codex` without spawning
/// real CLIs (which would mutate the developer's MCP registry).
/// Always returns `CliMissing`; the install flow then falls through to
/// printing the manual `claude mcp add` / `codex mcp add` hint, which
/// is the same observable behavior as today's pre-auto-registration
/// installer.
pub struct NoopMcpCli;

impl McpCli for NoopMcpCli {
    fn run(&self, _program: &str, _args: &[&str]) -> std::io::Result<Output> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

/// Build the `[--data-dir <path>] mcp` tail shared by both agents.
fn build_subprocess_args(data_dir: Option<&Path>) -> Vec<String> {
    match data_dir {
        Some(dd) => vec![
            "--data-dir".to_string(),
            dd.to_string_lossy().into_owned(),
            "mcp".to_string(),
        ],
        None => vec!["mcp".to_string()],
    }
}

/// `claude mcp remove --scope user aimx` → `claude mcp add --scope user
/// aimx -- /usr/local/bin/aimx [--data-dir <p>] mcp`.
///
/// The `--` separator is required: commander.js (Claude Code's CLI
/// parser) treats `--data-dir` as an option of `claude mcp add` itself
/// otherwise.
pub fn register_claude(cli: &dyn McpCli, data_dir: Option<&Path>) -> McpRegistration {
    let _ = cli.run("claude", &["mcp", "remove", "--scope", "user", "aimx"]);

    let tail = build_subprocess_args(data_dir);
    let mut args: Vec<&str> = vec!["mcp", "add", "--scope", "user", "aimx", "--", AIMX_BINARY];
    for a in &tail {
        args.push(a.as_str());
    }
    classify(cli.run("claude", &args))
}

/// `codex mcp remove aimx` → `codex mcp add aimx -- /usr/local/bin/aimx
/// [--data-dir <p>] mcp`. Codex CLI's MCP entries are user-scope by
/// default (written into `~/.codex/config.toml`); there is no `--scope`
/// flag on codex.
pub fn register_codex(cli: &dyn McpCli, data_dir: Option<&Path>) -> McpRegistration {
    let _ = cli.run("codex", &["mcp", "remove", "aimx"]);

    let tail = build_subprocess_args(data_dir);
    let mut args: Vec<&str> = vec!["mcp", "add", "aimx", "--", AIMX_BINARY];
    for a in &tail {
        args.push(a.as_str());
    }
    classify(cli.run("codex", &args))
}

fn classify(result: std::io::Result<Output>) -> McpRegistration {
    match result {
        Ok(out) if out.status.success() => McpRegistration::Registered,
        Ok(out) => {
            let exit_code = out.status.code();
            McpRegistration::RegisterFailed {
                stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
                exit_code,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => McpRegistration::CliMissing,
        Err(e) => McpRegistration::RegisterFailed {
            stderr: e.to_string(),
            exit_code: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    /// Captures every (program, args) pair the production code asks
    /// us to run, then replies with whatever `responses` is rigged to
    /// return. `responses` is consumed in call order.
    struct MockCli {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        // Stored reversed so `pop()` yields responses in call order.
        responses: RefCell<Vec<std::io::Result<Output>>>,
    }

    impl MockCli {
        fn new(responses: Vec<std::io::Result<Output>>) -> Self {
            let mut reversed = responses;
            reversed.reverse();
            Self {
                calls: RefCell::new(Vec::new()),
                responses: RefCell::new(reversed),
            }
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl McpCli for MockCli {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            self.responses
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| Err(std::io::Error::other("MockCli exhausted")))
        }
    }

    fn ok() -> std::io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn err(stderr: &str) -> std::io::Result<Output> {
        Ok(Output {
            // exit code 1 — `wait()`'s W*-shifted layout means we want
            // 1 << 8 to land in the `WEXITSTATUS` slot. `ExitStatus::from_raw`
            // takes the raw `wait()` status word.
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    fn not_found() -> std::io::Result<Output> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    #[test]
    fn register_claude_no_data_dir_uses_canonical_argv() {
        // Responses listed in call order: remove first, then add.
        let cli = MockCli::new(vec![ok(), ok()]);
        let result = register_claude(&cli, None);

        assert!(matches!(result, McpRegistration::Registered));
        let calls = cli.calls();
        assert_eq!(calls.len(), 2);

        assert_eq!(calls[0].0, "claude");
        assert_eq!(calls[0].1, vec!["mcp", "remove", "--scope", "user", "aimx"]);

        assert_eq!(calls[1].0, "claude");
        assert_eq!(
            calls[1].1,
            vec![
                "mcp",
                "add",
                "--scope",
                "user",
                "aimx",
                "--",
                "/usr/local/bin/aimx",
                "mcp",
            ]
        );
    }

    #[test]
    fn register_claude_with_data_dir_threads_path_into_argv() {
        let cli = MockCli::new(vec![ok(), ok()]);
        let dd = PathBuf::from("/var/lib/aimx-test");
        let result = register_claude(&cli, Some(&dd));

        assert!(matches!(result, McpRegistration::Registered));
        let calls = cli.calls();
        assert_eq!(
            calls[1].1,
            vec![
                "mcp",
                "add",
                "--scope",
                "user",
                "aimx",
                "--",
                "/usr/local/bin/aimx",
                "--data-dir",
                "/var/lib/aimx-test",
                "mcp",
            ]
        );
    }

    #[test]
    fn register_claude_propagates_cli_missing() {
        // Both calls fail with NotFound (claude not on PATH). The
        // remove-step error is best-effort-ignored independently of
        // whether the add-step error gets surfaced.
        let cli = MockCli::new(vec![not_found(), not_found()]);
        let result = register_claude(&cli, None);

        assert!(matches!(result, McpRegistration::CliMissing));
    }

    #[test]
    fn register_claude_propagates_register_failed_with_stderr() {
        let cli = MockCli::new(vec![ok(), err("server already exists\n")]);
        let result = register_claude(&cli, None);

        match result {
            McpRegistration::RegisterFailed { stderr, exit_code } => {
                assert_eq!(stderr, "server already exists");
                assert_eq!(exit_code, Some(1));
            }
            other => panic!("expected RegisterFailed, got {other:?}"),
        }
    }

    #[test]
    fn register_claude_register_failed_carries_exit_code_when_stderr_is_empty() {
        // Future Claude Code release that exits non-zero with no stderr —
        // the user still gets `(exit N)` for debugging.
        let cli = MockCli::new(vec![ok(), err("")]);
        let result = register_claude(&cli, None);

        match result {
            McpRegistration::RegisterFailed { stderr, exit_code } => {
                assert!(stderr.is_empty());
                assert_eq!(exit_code, Some(1));
            }
            other => panic!("expected RegisterFailed, got {other:?}"),
        }
    }

    #[test]
    fn register_claude_register_failed_from_io_error_has_no_exit_code() {
        // A spawn error other than NotFound (e.g. permission denied) is
        // surfaced as RegisterFailed without an exit code — the child
        // never ran.
        let cli = MockCli::new(vec![
            ok(),
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        ]);
        let result = register_claude(&cli, None);

        match result {
            McpRegistration::RegisterFailed { stderr, exit_code } => {
                assert!(!stderr.is_empty(), "should surface the io::Error message");
                assert_eq!(exit_code, None);
            }
            other => panic!("expected RegisterFailed, got {other:?}"),
        }
    }

    #[test]
    fn register_codex_no_data_dir_omits_scope_flag() {
        let cli = MockCli::new(vec![ok(), ok()]);
        let result = register_codex(&cli, None);

        assert!(matches!(result, McpRegistration::Registered));
        let calls = cli.calls();
        assert_eq!(calls.len(), 2);

        assert_eq!(calls[0].0, "codex");
        assert_eq!(calls[0].1, vec!["mcp", "remove", "aimx"]);

        assert_eq!(calls[1].0, "codex");
        assert_eq!(
            calls[1].1,
            vec!["mcp", "add", "aimx", "--", "/usr/local/bin/aimx", "mcp"]
        );
    }

    #[test]
    fn register_codex_with_data_dir_threads_path_into_argv() {
        let cli = MockCli::new(vec![ok(), ok()]);
        let dd = PathBuf::from("/srv/aimx");
        register_codex(&cli, Some(&dd));

        let calls = cli.calls();
        assert_eq!(
            calls[1].1,
            vec![
                "mcp",
                "add",
                "aimx",
                "--",
                "/usr/local/bin/aimx",
                "--data-dir",
                "/srv/aimx",
                "mcp",
            ]
        );
    }

    #[test]
    fn register_ignores_remove_failure_when_add_succeeds() {
        // remove returns NotFound (entry didn't exist), add returns ok.
        // Result should still be Registered — the remove-step error is
        // independent of the add-step outcome.
        let cli = MockCli::new(vec![not_found(), ok()]);
        let result = register_claude(&cli, None);

        assert!(matches!(result, McpRegistration::Registered));
    }
}
