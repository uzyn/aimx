//! Interactive checkbox TUI for `aimx agents setup`.
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
//! The target visual ("Claude Code / OpenClaw multi-select look") is:
//!
//! ```text
//! Setting up MCP integration for AI agents for `ubuntu`.
//! Select which AI agents you want to set up AIMX MCP for:
//!
//! ❯ [ ] Claude Code
//!   [x] Codex CLI  (AIMX MCP wired)
//!   [-] OpenClaw  (not detected)
//!   [ ] Gemini CLI
//!
//!   → Space toggles, Enter confirms, q cancels.
//! ```
//!
//! - Colored caret (`❯`) on the focused row.
//! - Filled `[x]` vs hollow `[ ]` for selected-vs-unselected.
//! - Dim `[-] ... (not detected)` for non-selectable rows.
//! - Clean left-aligned single column, no box-drawing.
//!
//! Non-TTY fallback: `run_with_env_to_writer` never calls this path when
//! `stderr()` isn't a TTY (see `agents_setup::is_tty_for_tui`) — scripts
//! get the plain registry dump instead. The TUI draws to `Term::stderr`
//! so detection on stderr matches where output actually lands.

use crate::agents_setup::{
    AgentEnv, InstallState, RunOpts, detect_install_state, registry, resolve_dest,
    run_with_env_post_gate,
};
use crate::term;
use console::{Key, Term, measure_text_width};
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
        .expect("agents setup TUI requires HOME to be resolved by the caller");
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

    // Non-blocker N2: when every row is `NotInstalled` the checkbox
    // widget is inert — Space does nothing on a non-selectable row, and
    // the operator has no explanation. Short-circuit with a clear
    // "install an agent first" message instead of rendering an empty TUI.
    if rows.iter().all(|r| !r.is_selectable()) {
        writeln!(
            out,
            "{}",
            term::header("No AI agents detected on this user's filesystem.")
        )?;
        writeln!(out)?;
        writeln!(
            out,
            "aimx checks for these agents under $HOME (and $XDG_CONFIG_HOME):"
        )?;
        for spec in registry() {
            writeln!(
                out,
                "  {} {}",
                term::dim("•"),
                term::highlight(spec.display_name)
            )?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "Install one of the above (e.g. Claude Code, Codex CLI, Gemini CLI, \
             OpenCode, Goose, OpenClaw, or Hermes) and re-run `aimx agents setup`."
        )?;
        return Ok(());
    }

    let term_handle = Term::stderr();
    // Resolve the invoking user via `getpwuid(geteuid())` so the TUI
    // header can show whose home directory the integration is being
    // wired into. This is best-effort — under unusual env (containers
    // with no passwd entry) we drop the trailing `for `<user>`` clause
    // entirely and just render `Setting up MCP integration for AI
    // agents.` instead of leaving an ugly placeholder behind.
    let username = env.caller_username();
    // Selection / confirmation loop: if the operator declines on the
    // confirmation screen we re-enter the picker with their previous
    // selections preserved so they don't lose work. Cancel from inside
    // the picker still aborts the whole flow.
    let mut current_rows = rows.clone();
    let selected_rows = loop {
        let picked = match interact(&term_handle, &current_rows, username.as_deref()) {
            Ok(Some(rs)) => rs,
            Ok(None) => {
                // User cancelled with `q` or Ctrl-C.
                writeln!(out, "\nCancelled. No agents were wired.")?;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        if picked.is_empty() {
            writeln!(out, "\nNo agents selected. Nothing to do.")?;
            return Ok(());
        }

        render_confirmation(out, &picked)?;
        if read_confirm_yn(&term_handle)? {
            break picked;
        }
        // Operator declined: preserve their selections (carry the
        // `selected` flag forward) and re-enter the picker.
        let picked_indices: std::collections::HashSet<usize> =
            picked.iter().map(|r| r.spec_index).collect();
        for row in current_rows.iter_mut() {
            row.selected = picked_indices.contains(&row.spec_index);
        }
    };

    // For each selected agent, invoke the post-root-gate install path
    // directly. The root gate has already fired in `run_with_env_to_writer`
    // (and, when `--dangerously-allow-root` is set, wrapped the ambient
    // env in `OverrideHomeEnv` once). Calling `run_with_env` here would
    // re-apply the gate and re-wrap, so we skip it — see non-blocker N6.
    //
    // Failures are captured into the summary table rather than aborting
    // the loop so a single broken agent doesn't silently prevent the
    // others from being reported.
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
            no_interactive: true,
            // Already inside the post-gate path; the inner function
            // doesn't inspect this flag again.
            dangerously_allow_root: opts.dangerously_allow_root,
            data_dir: opts.data_dir,
        };
        let outcome = run_with_env_post_gate(sub_opts, env, out);

        let (status_display, record) = match outcome {
            Ok(()) => (
                format!("{} ok", term::success_mark()),
                Ok(dest.to_string_lossy().into_owned()),
            ),
            Err(e) => {
                writeln!(out, "  {} {}", term::fail_mark(), e)?;
                (format!("{} failed", term::fail_mark()), Err(e.to_string()))
            }
        };
        results.push((spec.display_name.to_string(), status_display, record));
    }

    writeln!(out)?;
    writeln!(out, "{}", term::header("Summary"))?;
    writeln!(out)?;
    render_summary_table(out, &results)?;
    Ok(())
}

/// Render the `agent | status | destination` summary table. Uses
/// `console::measure_text_width` for column widths so ANSI escape
/// sequences in the status column don't inflate the padding — N5 from
/// PR #139 review.
fn render_summary_table(
    out: &mut dyn Write,
    results: &[(String, String, Result<String, String>)],
) -> io::Result<()> {
    let header_agent = "Agent";
    let header_status = "Status";
    let header_dest = "Destination";

    let name_col = results
        .iter()
        .map(|(name, _, _)| measure_text_width(name))
        .max()
        .unwrap_or(0)
        .max(measure_text_width(header_agent));
    let status_col = results
        .iter()
        .map(|(_, status, _)| measure_text_width(status))
        .max()
        .unwrap_or(0)
        .max(measure_text_width(header_status));

    writeln!(
        out,
        "  {}  {}  {header_dest}",
        pad_visible(header_agent, name_col),
        pad_visible(header_status, status_col)
    )?;
    writeln!(
        out,
        "  {}  {}  {}",
        "-".repeat(name_col),
        "-".repeat(status_col),
        "-".repeat(measure_text_width(header_dest))
    )?;
    for (name, status_display, record) in results {
        let dest = match record {
            Ok(p) => p.clone(),
            Err(e) => format!("(error: {e})"),
        };
        writeln!(
            out,
            "  {}  {}  {dest}",
            pad_visible(name, name_col),
            pad_visible(status_display, status_col)
        )?;
    }
    Ok(())
}

/// Left-pad a (possibly ANSI-styled) string to `width` visible columns.
/// `format!("{s:<width$}")` counts bytes, so colored tokens like
/// `✓ ok` would be under-padded. This helper pads based on the visible
/// width reported by `console::measure_text_width`.
fn pad_visible(s: &str, width: usize) -> String {
    let visible = measure_text_width(s);
    if visible >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(s.len() + (width - visible));
        out.push_str(s);
        for _ in 0..(width - visible) {
            out.push(' ');
        }
        out
    }
}

/// Drive the cursor loop. Returns `Ok(Some(rows))` on Enter, `Ok(None)`
/// on cancel (`q`, `Ctrl-C`, `Esc`), or `Err` on terminal I/O failure.
fn interact(
    term_handle: &Term,
    rows: &[Row],
    username: Option<&str>,
) -> io::Result<Option<Vec<Row>>> {
    let mut rows = rows.to_vec();
    // Place the cursor on the first selectable row. If none exist, the
    // operator can only cancel.
    let mut cursor = rows.iter().position(|r| r.is_selectable()).unwrap_or(0);

    let mut first_draw = true;
    loop {
        if !first_draw {
            // Move the cursor up to redraw the menu in place. The header
            // takes 3 lines (banner + sub-line + blank), each row is 1
            // line, and the trailing hint above the cursor is 1 line.
            term_handle.clear_last_lines(rows.len() + 4)?;
        }
        first_draw = false;

        render(term_handle, &rows, cursor, username)?;

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

/// Render the confirmation screen between picker selection and the
/// install loop. Lists each selected agent with the right verb:
/// "Install" for `InstalledNotWired` (fresh wiring) and "Re-install"
/// for `InstalledWired` (refresh existing files). The trailing prompt
/// `Confirm? [Y/n]` is appended without a trailing newline so the
/// caller's read mirrors the wizard's `[Y/n]` prompt convention.
pub(crate) fn render_confirmation(out: &mut dyn Write, picked: &[Row]) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "{}", term::header("Tasks to perform:"))?;
    writeln!(out)?;
    for (idx, row) in picked.iter().enumerate() {
        let spec = &registry()[row.spec_index];
        let verb = match row.state {
            InstallState::InstalledWired => "Re-install AIMX MCP for",
            // InstalledNotWired (and any future selectable state) gets
            // the plain Install verb — fresh wiring.
            _ => "Install AIMX MCP for",
        };
        let suffix = match row.state {
            InstallState::InstalledWired => format!(" {}", term::dim("(refresh files)")),
            _ => String::new(),
        };
        writeln!(
            out,
            "  {}. {} {}{suffix}",
            idx + 1,
            verb,
            term::highlight(spec.display_name)
        )?;
    }
    writeln!(out)?;
    write!(out, "Confirm? [Y/n] ")?;
    out.flush()?;
    Ok(())
}

/// Read a single line from the controlling terminal and apply the same
/// `[Y/n]` default-yes parsing as the wizard. Lives next to
/// [`render_confirmation`] so the picker → confirm → install flow uses
/// one consistent rule.
fn read_confirm_yn(term_handle: &Term) -> io::Result<bool> {
    let line = term_handle.read_line()?;
    Ok(parse_confirm_yn(&line))
}

/// Pure parser for `[Y/n]` answers. Mirrors `setup::parse_yn`: blank
/// Enter or any non-`n`/`N` first character → `true`; `n`/`N` → `false`.
/// Kept private to the TUI so the confirm screen has its own pinned
/// behavior independent of the wizard's parser.
pub(crate) fn parse_confirm_yn(input: &str) -> bool {
    !input
        .trim()
        .chars()
        .next()
        .is_some_and(|c| c == 'n' || c == 'N')
}

fn render(
    term_handle: &Term,
    rows: &[Row],
    cursor: usize,
    username: Option<&str>,
) -> io::Result<()> {
    let header_line = match username {
        Some(u) => format!("Setting up MCP integration for AI agents for `{u}`."),
        None => "Setting up MCP integration for AI agents.".to_string(),
    };
    term_handle.write_line(&term::header(&header_line).to_string())?;
    term_handle.write_line("Select which AI agents you want to set up AIMX MCP for:")?;
    term_handle.write_line("")?;
    for (i, row) in rows.iter().enumerate() {
        let spec = &registry()[row.spec_index];
        let caret = if i == cursor && row.is_selectable() {
            // Copper `❯` on the focused row per branding §5.4 — the
            // prompt/navigation accent is the one surface that uses the
            // brand's accent color. Non-selectable rows never receive the
            // caret (the cursor skips them).
            term::accent("❯").to_string()
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
        // Two-shade dim per branding §5.4: agent name carries one
        // dim level (when non-selectable) while the status suffix is
        // one shade deeper still, so the operator can immediately tell
        // the suffix from the name even on rows where both are dim.
        let name = match row.state {
            InstallState::NotInstalled => term::dim(spec.display_name).to_string(),
            InstallState::InstalledWired => {
                format!(
                    "{}  {}",
                    spec.display_name,
                    term::very_dim("(AIMX MCP wired)")
                )
            }
            InstallState::InstalledNotWired => spec.display_name.to_string(),
        };
        let suffix = match row.state {
            InstallState::NotInstalled => format!(" {}", term::very_dim("(not detected)")),
            _ => String::new(),
        };
        term_handle.write_line(&format!("{caret} {mark} {name}{suffix}"))?;
    }
    // Hint sits BELOW the rows, right above the cursor — that's where
    // it's most useful (operators read top-down then act).
    term_handle.write_line(&format!(
        "  {} {}",
        term::prompt_mark(),
        term::dim("Space toggles, Enter confirms, q cancels.")
    ))?;
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
        // Create `~/.claude/plugins/aimx/.claude-plugin/plugin.json`
        // (the canonical Claude-Code plugin manifest aimx writes) so
        // detection reports InstalledWired. Row is selectable (operator
        // can choose to re-wire / overwrite) but defaults to unselected
        // to avoid unnecessary re-installs.
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("aimx")
            .join(".claude-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.json"), r#"{"name":"aimx"}"#).unwrap();
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

    // Non-blocker N2: the all-NotInstalled short-circuit must render an
    // explanation instead of launching an inert TUI. We exercise the
    // guidance path directly via `run_tui` + an empty home — `interact`
    // would otherwise block on `Term::stderr().read_key()`.
    #[test]
    fn run_tui_prints_guidance_when_no_agents_detected() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv::new(tmp.path().to_path_buf());
        let opts = RunOpts {
            agent: None,
            list: false,
            force: false,
            print: false,
            no_interactive: false,
            dangerously_allow_root: false,
            data_dir: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        run_tui(&opts, &env, &mut buf).expect("all-NotInstalled path must not error");
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("No AI agents detected"),
            "expected helper banner, got: {out}"
        );
        assert!(
            out.contains("Claude Code"),
            "expected agent name list, got: {out}"
        );
        assert!(
            out.contains("re-run `aimx agents setup`"),
            "expected retry hint, got: {out}"
        );
    }

    // Non-blocker N5: summary-table rows must be visually aligned when
    // the status column contains ANSI escapes. Force-enable colors so
    // `term::success("✓")` emits escape codes, then assert the visible
    // column width is identical on every row.
    #[test]
    fn summary_table_columns_align_with_ansi_escapes() {
        use colored::control;
        // `control::set_override` is process-global; keep the override
        // local to this test and clean up immediately.
        control::set_override(true);
        let results = vec![
            (
                "Claude Code".to_string(),
                format!("{} ok", term::success_mark()),
                Ok::<String, String>("/home/u/.claude/plugins/aimx".to_string()),
            ),
            (
                "Codex CLI".to_string(),
                format!("{} failed", term::fail_mark()),
                Err::<String, String>("boom".to_string()),
            ),
        ];
        let mut buf: Vec<u8> = Vec::new();
        render_summary_table(&mut buf, &results).unwrap();
        control::unset_override();

        let rendered = String::from_utf8(buf).unwrap();
        // Strip the 2-space leading indent, split on rows.
        let lines: Vec<&str> = rendered.lines().collect();
        // header, separator, row1, row2
        assert_eq!(lines.len(), 4, "expected 4 lines, got:\n{rendered}");

        // The visible byte positions of the `Destination` column on the
        // header row and the two data rows must match. We compute the
        // visible-width prefix up to the start of the destination token.
        // Destination in row1 starts with `/home`; in header starts with
        // `Destination`; in row2 starts with `(error:`.
        fn visible_prefix_before(line: &str, token: &str) -> usize {
            let pos = line.find(token).expect("token not found");
            measure_text_width(&line[..pos])
        }
        let header_col = visible_prefix_before(lines[0], "Destination");
        let row1_col = visible_prefix_before(lines[2], "/home");
        let row2_col = visible_prefix_before(lines[3], "(error:");
        assert_eq!(
            header_col, row1_col,
            "header and row 1 Destination columns misaligned ({header_col} vs {row1_col})\n{rendered}"
        );
        assert_eq!(
            header_col, row2_col,
            "header and row 2 Destination columns misaligned ({header_col} vs {row2_col})\n{rendered}"
        );
    }

    #[test]
    fn pad_visible_ignores_ansi_escapes() {
        let plain = pad_visible("ok", 6);
        assert_eq!(plain, "ok    ");
        let colored_ok = format!("{}", term::success("ok"));
        // Plain-colored: when colors are disabled globally, the output
        // matches the plain padding; when they're enabled, the visible
        // width is still 2 so padding still produces 4 trailing spaces.
        let padded = pad_visible(&colored_ok, 6);
        assert_eq!(
            measure_text_width(&padded),
            6,
            "padded visible width must equal target"
        );
    }

    // ----- Cycle 5: header rewording + username surface ---------------------

    #[test]
    fn render_header_uses_new_spec_wording() {
        // The user-visible header must match the cycle-5 wording
        // exactly so the operator immediately sees what `aimx
        // agents setup` is for and whose home it's writing to. We walk
        // just the `render` body (not the whole file) so this test's
        // own asserts can mention the new strings without
        // self-tripping a "old wording must not appear" check.
        let source = include_str!("agents_tui.rs");
        let render_start = source.find("fn render(").expect("render fn must exist");
        let render_end = source[render_start..]
            .find("\nfn ")
            .map(|off| render_start + off)
            .unwrap_or(source.len());
        let body = &source[render_start..render_end];
        assert!(
            body.contains("Setting up MCP integration for AI agents for"),
            "render must emit the new header wording (line 1): {body}"
        );
        assert!(
            body.contains("Select which AI agents you want to set up AIMX MCP for:"),
            "render must emit the new selection sub-line (line 2): {body}"
        );
        // Build the forbidden literal at runtime so this test's source
        // doesn't trip its own check.
        let forbidden = ["Wire", " aimx", " into", " your", " AI", " agents"].concat();
        assert!(
            !body.contains(&forbidden),
            "render must NOT carry the old header wording"
        );
    }

    #[test]
    fn render_threads_username_into_header() {
        // The username comes from `AgentEnv::caller_username()` and
        // gets formatted as a backtick-quoted token in the header
        // when present. When `caller_username()` returns None, the
        // header drops the trailing `for `<user>`` clause entirely —
        // no placeholder is shown.
        let source = include_str!("agents_tui.rs");
        assert!(
            source.contains("env.caller_username()"),
            "run_tui must surface the invoking username via AgentEnv"
        );
        assert!(
            source.contains("for `{u}`."),
            "render must format the username as a backtick-quoted token"
        );
    }

    #[test]
    fn render_omits_for_clause_when_username_is_none() {
        // When `caller_username()` returns None we render the clean
        // header line `Setting up MCP integration for AI agents.` —
        // no backtick, no placeholder, no trailing `for ...` clause.
        // Walk just the `render` body (top-level `fn render(` to its
        // matching closing `}` at column 1) so test code that mentions
        // the old placeholder doesn't self-trip.
        let source = include_str!("agents_tui.rs");
        let render_start = source.find("fn render(").expect("render fn must exist");
        // First column-1 `}\n` after the opening — that's where the
        // function body closes (private fns in this module sit at
        // column 0, so a `}` at column 0 reliably terminates).
        let render_end = source[render_start..]
            .find("\n}\n")
            .map(|off| render_start + off + "\n}\n".len())
            .expect("render body must close with `}` at column 1");
        let body = &source[render_start..render_end];
        // The no-username branch must produce the clean fallback
        // header string (no `for ...` clause, no placeholder).
        assert!(
            body.contains("Setting up MCP integration for AI agents."),
            "render body must include the no-username fallback header: {body}"
        );
        // The forbidden `<user>` placeholder, built at runtime so this
        // test's source doesn't trip its own check.
        let forbidden = ["<", "user", ">"].concat();
        assert!(
            !body.contains(&forbidden),
            "render body must not carry the literal placeholder anymore: {body}"
        );
        // Smoke: render(.., username=None, ..) must not panic.
        let term_handle = Term::stdout();
        let rows: Vec<Row> = Vec::new();
        let _ = render(&term_handle, &rows, 0, None);
    }

    /// Walk just `fn render(...)`'s top-level body — `fn render(` to the
    /// matching column-0 `\n}\n` — so source-grep tests can assert on the
    /// production render function without picking up the rest of the file
    /// (including these tests, whose bodies otherwise self-trip on the
    /// "(already wired)" / "term::very_dim" tokens).
    fn render_fn_body() -> &'static str {
        let source = include_str!("agents_tui.rs");
        let start = source.find("fn render(").expect("render fn must exist");
        let end = source[start..]
            .find("\n}\n")
            .map(|off| start + off + "\n}\n".len())
            .expect("render body must close with `}` at column 1");
        // SAFETY: `include_str!` returns a `&'static str`; the slice is
        // pinned for the program's lifetime.
        &source[start..end]
    }

    #[test]
    fn render_emits_aimx_mcp_wired_label() {
        // The "wired" status suffix changed from `(already wired)` to
        // `(AIMX MCP wired)` for consistency with the "wire" verb used
        // elsewhere. Source-grep the render fn body so this test runs
        // without needing a TTY.
        let body = render_fn_body();
        assert!(
            body.contains("(AIMX MCP wired)"),
            "render must emit the new wired label: {body}"
        );
        // Build the forbidden literal at runtime so this test's source
        // doesn't trip its own check.
        let forbidden = ["(already", " ", "wired)"].concat();
        assert!(
            !body.contains(&forbidden),
            "render must not carry the old wired label: {body}"
        );
    }

    #[test]
    fn render_uses_very_dim_for_status_suffix() {
        // The status suffix (`(AIMX MCP wired)`, `(not detected)`) must
        // wrap through `term::very_dim` so it sits one shade deeper than
        // the agent display name beside it. Source-grep the render fn
        // body for the helper invocation.
        let body = render_fn_body();
        assert!(
            body.contains("term::very_dim(\"(AIMX MCP wired)\")"),
            "render must wrap the wired suffix in term::very_dim: {body}"
        );
        assert!(
            body.contains("term::very_dim(\"(not detected)\")"),
            "render must wrap the not-detected suffix in term::very_dim: {body}"
        );
    }

    #[test]
    fn confirmation_screen_lists_install_and_reinstall() {
        // Build a known TUI selection: one InstalledNotWired (fresh
        // install) and one InstalledWired (re-install). Render the
        // confirmation screen and assert each task carries the right
        // verb and the affected agent's display name.
        let claude_index = registry()
            .iter()
            .position(|s| s.name == "claude-code")
            .unwrap();
        let codex_index = registry().iter().position(|s| s.name == "codex").unwrap();
        let picked = vec![
            Row {
                spec_index: codex_index,
                state: InstallState::InstalledNotWired,
                selected: true,
            },
            Row {
                spec_index: claude_index,
                state: InstallState::InstalledWired,
                selected: true,
            },
        ];
        let mut buf: Vec<u8> = Vec::new();
        render_confirmation(&mut buf, &picked).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Tasks to perform:"),
            "expected confirmation header, got: {out}"
        );
        assert!(
            out.contains("Install AIMX MCP for"),
            "fresh-install verb missing, got: {out}"
        );
        assert!(
            out.contains("Re-install AIMX MCP for"),
            "re-install verb missing, got: {out}"
        );
        assert!(
            out.contains("Codex CLI"),
            "expected Codex CLI display name, got: {out}"
        );
        assert!(
            out.contains("Claude Code"),
            "expected Claude Code display name, got: {out}"
        );
        assert!(
            out.contains("Confirm? [Y/n]"),
            "expected confirmation prompt, got: {out}"
        );
    }

    #[test]
    fn confirmation_screen_returns_to_selection_on_no() {
        // The pure parser drives the loop control. Feeding `n` /
        // `no` / `N` produces `false`, which `run_tui` interprets as
        // "go back to picker". Anything else (blank, `y`, `yes`,
        // garbage) confirms.
        assert!(!parse_confirm_yn("n"));
        assert!(!parse_confirm_yn("no"));
        assert!(!parse_confirm_yn("N"));
        assert!(!parse_confirm_yn("  no  "));
        // Default-yes semantics.
        assert!(parse_confirm_yn(""));
        assert!(parse_confirm_yn("\n"));
        assert!(parse_confirm_yn("y"));
        assert!(parse_confirm_yn("yes"));
        assert!(parse_confirm_yn("Y"));
    }

    #[test]
    fn run_tui_loop_returns_to_picker_on_confirm_no() {
        // Source-grep: the confirmation step lives inside a `loop` so a
        // `false` answer continues to the next iteration (re-entering
        // the picker), while `true` `break`s out with the picked rows.
        // The previous selections are carried forward via a recompute
        // of `row.selected` against the picked-spec indices, not by
        // overwriting them with the freshly-built defaults.
        let source = include_str!("agents_tui.rs");
        let run_start = source.find("pub fn run_tui(").expect("run_tui must exist");
        let run_end = source[run_start..]
            .find("\nfn ")
            .map(|off| run_start + off)
            .unwrap_or(source.len());
        let body = &source[run_start..run_end];
        assert!(
            body.contains("render_confirmation(out, &picked)"),
            "run_tui must render confirmation between selection and install: {body}"
        );
        assert!(
            body.contains("read_confirm_yn(&term_handle)"),
            "run_tui must read [Y/n] via the confirmation helper: {body}"
        );
        assert!(
            body.contains("break picked"),
            "run_tui must break out of the picker loop on Yes: {body}"
        );
        // The "no" branch must not zero out the operator's prior
        // selections — we recompute `selected` against the picked set,
        // we don't rebuild rows from scratch.
        assert!(
            body.contains("picked_indices"),
            "run_tui must carry prior selections forward on No: {body}"
        );
    }

    #[test]
    fn render_hint_sits_below_rows() {
        // The "Space toggles, Enter confirms, q cancels." hint moved
        // from the header band to a line BELOW the rows — right above
        // the cursor where it's most useful. Source-grep both
        // invariants: the hint string is present, and it's emitted
        // AFTER the row loop in `render`.
        let source = include_str!("agents_tui.rs");
        let render_start = source.find("fn render(").expect("render fn must exist");
        let render_end = source[render_start..]
            .find("\nfn ")
            .map(|off| render_start + off)
            .unwrap_or(source.len());
        let body = &source[render_start..render_end];
        let hint_pos = body
            .find("Space toggles, Enter confirms, q cancels.")
            .expect("hint must exist in render");
        let row_loop_pos = body
            .find("for (i, row) in rows.iter().enumerate()")
            .expect("row loop must exist in render");
        assert!(
            hint_pos > row_loop_pos,
            "hint must come AFTER the row loop (hint @ {hint_pos}, row loop @ {row_loop_pos})"
        );
    }
}
