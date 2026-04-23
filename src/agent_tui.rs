//! Interactive checkbox TUI for `aimx agent-setup` (Sprint 6 / FR-5.1–5.5).
//!
//! Hand-rolled renderer on top of `console::Term` / `console::Key`. Used
//! instead of `dialoguer::MultiSelect` because:
//!
//! 1. `MultiSelect` ships the default blue-bold theme which conflicts
//!    with `docs/branding.md` §5.4 (copper accent, `✓ ✗ ⚠ →` marks,
//!    dim non-selectable rows).
//! 2. `MultiSelect` does not model non-selectable rows cleanly — every
//!    item is either togglable or absent. We need an `[-] (not
//!    detected)` row that the cursor skips over.
//!
//! The target visual (sprint S6-1 "Claude Code / OpenClaw multi-select
//! look") is:
//!
//! ```text
//!   Wire aimx into your AI agents — Space toggles, Enter confirms, q cancels.
//!
//! ❯ [ ] Claude Code
//!   [x] Codex CLI  (already wired)
//!   [-] OpenClaw  (not detected)
//!   [ ] Gemini CLI
//! ```
//!
//! - Colored caret (`❯`) on the focused row.
//! - Filled `[x]` vs hollow `[ ]` for selected-vs-unselected.
//! - Dim `[-] ... (not detected)` for non-selectable rows.
//! - Clean left-aligned single column, no box-drawing.
//!
//! Non-TTY fallback: `run_with_env_to_writer` never calls this path when
//! `stdout()` isn't a TTY (see `agent_setup::is_stdout_tty`) — scripts
//! get the plain registry dump instead.

use crate::agent_setup::{
    AgentEnv, InstallState, RunOpts, detect_install_state, registry, resolve_dest, run_with_env,
};
use crate::term;
use console::{Key, Term};
use std::io::{self, Write};

/// One row in the TUI.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    spec_index: usize,
    state: InstallState,
    selected: bool,
}

impl Row {
    fn is_selectable(&self) -> bool {
        !matches!(self.state, InstallState::NotInstalled)
    }
}

/// Snapshot produced by [`build_rows`]: every agent in the registry with
/// its detected install state and default selection. Split out so unit
/// tests can assert on the detection → default-selection mapping without
/// spinning up a `Term`.
pub(crate) fn build_rows(env: &dyn AgentEnv) -> Vec<Row> {
    let home = env
        .home_dir()
        .expect("agent-setup TUI requires HOME to be resolved by the caller");
    let xdg = env.xdg_config_home();
    registry()
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let state = detect_install_state(spec, &home, xdg.clone());
            let selected = matches!(state, InstallState::InstalledNotWired);
            Row {
                spec_index: i,
                state,
                selected,
            }
        })
        .collect()
}

/// Entry point called by `run_with_env_to_writer` when no agent argument
/// is passed and stdout is a TTY. Renders the checkbox menu, blocks on
/// the operator's selection, then fires `run_with_env` per selected
/// agent and writes a summary table to `out`.
pub fn run_tui(
    opts: &RunOpts<'_>,
    env: &dyn AgentEnv,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = build_rows(env);

    let term_handle = Term::stderr();
    let selected_rows = match interact(&term_handle, &rows) {
        Ok(Some(rs)) => rs,
        Ok(None) => {
            // User cancelled with `q` or Ctrl-C.
            writeln!(out, "\nCancelled. No agents were wired.")?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    if selected_rows.is_empty() {
        writeln!(out, "\nNo agents selected. Nothing to do.")?;
        return Ok(());
    }

    // For each selected agent, re-run `run_with_env` with the positional
    // `<agent>` argument. Failures are captured into the summary table
    // rather than aborting the loop so a single broken agent doesn't
    // silently prevent the others from being reported.
    let mut results: Vec<(String, String, Result<String, String>)> = Vec::new();
    for row in &selected_rows {
        let spec = &registry()[row.spec_index];
        let dest = resolve_dest(spec.dest_template, env)
            .unwrap_or_else(|_| std::path::PathBuf::from(spec.dest_template));

        writeln!(out)?;
        writeln!(
            out,
            "{} {}",
            term::header("==>"),
            term::highlight(spec.display_name)
        )?;

        let sub_opts = RunOpts {
            agent: Some(spec.name.to_string()),
            list: false,
            force: opts.force,
            print: false,
            no_template: opts.no_template,
            redetect: opts.redetect,
            no_interactive: true,
            dangerously_allow_root: opts.dangerously_allow_root,
            data_dir: opts.data_dir,
        };
        let outcome = run_with_env(sub_opts, env);

        let (status_display, record) = match outcome {
            Ok(()) => (
                format!("{} ok", term::success("✓")),
                Ok(dest.to_string_lossy().into_owned()),
            ),
            Err(e) => {
                writeln!(out, "  {} {}", term::error("✗"), e)?;
                (format!("{} failed", term::error("✗")), Err(e.to_string()))
            }
        };
        results.push((spec.display_name.to_string(), status_display, record));
    }

    writeln!(out)?;
    writeln!(out, "{}", term::header("Summary"))?;
    writeln!(out)?;
    // Two-column table: `agent | status | destination`. Column widths are
    // computed against the selected rows so the `|` separators line up.
    let name_col = results
        .iter()
        .map(|(name, _, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max("Agent".len());
    let status_col = "failed".len() + 2; // "✓ ok" / "✗ failed" rendered width
    let header_agent = "Agent";
    let header_status = "Status";
    let header_dest = "Destination";
    writeln!(
        out,
        "  {header_agent:<name_col$}  {header_status:<status_col$}  {header_dest}"
    )?;
    writeln!(
        out,
        "  {}  {}  {}",
        "-".repeat(name_col),
        "-".repeat(status_col),
        "-".repeat(16)
    )?;
    for (name, status_display, record) in &results {
        let dest = match record {
            Ok(p) => p.clone(),
            Err(e) => format!("(error: {e})"),
        };
        writeln!(
            out,
            "  {name:<name_col$}  {status_display:<status_col$}  {dest}"
        )?;
    }
    Ok(())
}

/// Drive the cursor loop. Returns `Ok(Some(rows))` on Enter, `Ok(None)`
/// on cancel (`q`, `Ctrl-C`, `Esc`), or `Err` on terminal I/O failure.
fn interact(term_handle: &Term, rows: &[Row]) -> io::Result<Option<Vec<Row>>> {
    let mut rows = rows.to_vec();
    // Place the cursor on the first selectable row. If none exist, the
    // operator can only cancel.
    let mut cursor = rows.iter().position(|r| r.is_selectable()).unwrap_or(0);

    let mut first_draw = true;
    loop {
        if !first_draw {
            // Move the cursor up to redraw the menu in place. The header
            // takes 2 lines and each row is 1 line.
            term_handle.clear_last_lines(rows.len() + 2)?;
        }
        first_draw = false;

        render(term_handle, &rows, cursor)?;

        let key = term_handle.read_key()?;
        match key {
            Key::ArrowDown | Key::Char('j') => {
                cursor = next_selectable(&rows, cursor, 1).unwrap_or(cursor);
            }
            Key::ArrowUp | Key::Char('k') => {
                cursor = next_selectable(&rows, cursor, -1).unwrap_or(cursor);
            }
            Key::Char(' ') => {
                if let Some(row) = rows.get_mut(cursor)
                    && row.is_selectable()
                {
                    row.selected = !row.selected;
                }
            }
            Key::Enter => {
                return Ok(Some(rows.into_iter().filter(|r| r.selected).collect()));
            }
            Key::Char('q') | Key::Escape | Key::CtrlC => return Ok(None),
            _ => {}
        }
    }
}

fn next_selectable(rows: &[Row], start: usize, delta: isize) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let n = rows.len() as isize;
    let mut i = start as isize;
    for _ in 0..rows.len() {
        i = (i + delta).rem_euclid(n);
        if rows[i as usize].is_selectable() {
            return Some(i as usize);
        }
    }
    None
}

fn render(term_handle: &Term, rows: &[Row], cursor: usize) -> io::Result<()> {
    term_handle.write_line(&term::header("Wire aimx into your AI agents").to_string())?;
    term_handle.write_line(&format!(
        "  {} Space toggles, Enter confirms, q cancels.",
        term::dim("→")
    ))?;
    for (i, row) in rows.iter().enumerate() {
        let spec = &registry()[row.spec_index];
        let caret = if i == cursor && row.is_selectable() {
            // Colored caret on the focused row. Uses the same success-green
            // as other markers until Sprint 7 introduces the copper accent
            // helper in `term.rs`. Non-selectable rows never receive the
            // caret (the cursor skips them).
            term::success("❯").to_string()
        } else {
            " ".to_string()
        };
        let mark = match row.state {
            InstallState::NotInstalled => term::dim("[-]").to_string(),
            _ => {
                if row.selected {
                    term::highlight("[x]").to_string()
                } else {
                    "[ ]".to_string()
                }
            }
        };
        let name = match row.state {
            InstallState::NotInstalled => term::dim(spec.display_name).to_string(),
            InstallState::InstalledWired => {
                format!("{}  {}", spec.display_name, term::dim("(already wired)"))
            }
            InstallState::InstalledNotWired => spec.display_name.to_string(),
        };
        let suffix = match row.state {
            InstallState::NotInstalled => format!(" {}", term::dim("(not detected)")),
            _ => String::new(),
        };
        term_handle.write_line(&format!("{caret} {mark} {name}{suffix}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct FakeEnv {
        home: PathBuf,
        xdg: Option<PathBuf>,
        _responses: RefCell<Vec<String>>,
    }

    impl FakeEnv {
        fn new(home: PathBuf) -> Self {
            Self {
                home,
                xdg: None,
                _responses: RefCell::new(Vec::new()),
            }
        }
    }

    impl AgentEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn xdg_config_home(&self) -> Option<PathBuf> {
            self.xdg.clone()
        }
        fn is_root(&self) -> bool {
            false
        }
        fn is_stdin_tty(&self) -> bool {
            false
        }
        fn read_line(&self) -> io::Result<String> {
            Err(io::Error::other("not used in TUI tests"))
        }
    }

    #[test]
    fn build_rows_defaults_to_not_installed_on_empty_home() {
        // A pristine tempdir has no agent directories → every row is
        // NotInstalled → every row is non-selectable and unselected.
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv::new(tmp.path().to_path_buf());
        let rows = build_rows(&env);
        assert_eq!(rows.len(), registry().len());
        for row in &rows {
            assert!(matches!(row.state, InstallState::NotInstalled));
            assert!(!row.selected);
            assert!(!row.is_selectable());
        }
    }

    #[test]
    fn build_rows_defaults_selected_on_installed_not_wired() {
        // Create `~/.claude` (agent_root for claude-code) but not
        // `~/.claude/plugins/aimx`. Claude Code row should default to
        // selected, others NotInstalled.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let env = FakeEnv::new(tmp.path().to_path_buf());
        let rows = build_rows(&env);
        let claude_row = rows
            .iter()
            .find(|r| registry()[r.spec_index].name == "claude-code")
            .unwrap();
        assert!(matches!(claude_row.state, InstallState::InstalledNotWired));
        assert!(claude_row.selected, "installed-not-wired defaults selected");
    }

    #[test]
    fn build_rows_already_wired_is_unselected_but_selectable() {
        // Create `~/.claude/plugins/aimx` so detection reports
        // InstalledWired. Row is selectable (operator can choose to
        // re-wire / overwrite) but defaults to unselected to avoid
        // unnecessary re-installs.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude").join("plugins").join("aimx")).unwrap();
        let env = FakeEnv::new(tmp.path().to_path_buf());
        let rows = build_rows(&env);
        let claude_row = rows
            .iter()
            .find(|r| registry()[r.spec_index].name == "claude-code")
            .unwrap();
        assert!(matches!(claude_row.state, InstallState::InstalledWired));
        assert!(!claude_row.selected);
        assert!(claude_row.is_selectable());
    }

    #[test]
    fn next_selectable_skips_not_installed_rows() {
        let rows = vec![
            Row {
                spec_index: 0,
                state: InstallState::NotInstalled,
                selected: false,
            },
            Row {
                spec_index: 1,
                state: InstallState::InstalledNotWired,
                selected: true,
            },
            Row {
                spec_index: 2,
                state: InstallState::NotInstalled,
                selected: false,
            },
            Row {
                spec_index: 3,
                state: InstallState::InstalledWired,
                selected: false,
            },
        ];
        // Starting at row 1, "down" skips row 2 and lands on row 3.
        assert_eq!(next_selectable(&rows, 1, 1), Some(3));
        // Starting at row 3, "up" wraps past rows 2/0 to row 1.
        assert_eq!(next_selectable(&rows, 3, -1), Some(1));
    }

    #[test]
    fn next_selectable_returns_none_when_no_selectable_rows_exist() {
        let rows = vec![Row {
            spec_index: 0,
            state: InstallState::NotInstalled,
            selected: false,
        }];
        assert_eq!(next_selectable(&rows, 0, 1), None);
        assert_eq!(next_selectable(&rows, 0, -1), None);
    }
}
