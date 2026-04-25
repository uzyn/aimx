//! Interactive checkbox TUI for `aimx agent-setup`.
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
//!   [x] Codex CLI  (already wired)
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
//! `stderr()` isn't a TTY (see `agent_setup::is_tty_for_tui`) — scripts
//! get the plain registry dump instead. The TUI draws to `Term::stderr`
//! so detection on stderr matches where output actually lands.

use crate::agent_setup::{
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
             OpenCode, Goose, OpenClaw, or Hermes) and re-run `aimx agent-setup`."
        )?;
        return Ok(());
    }

    let term_handle = Term::stderr();
    // Resolve the invoking user via `getpwuid(geteuid())` so the TUI
    // header can show whose home directory the integration is being
    // wired into. This is best-effort — under unusual env (containers
    // with no passwd entry) we fall back to a literal `<user>` token.
    let username = env.caller_username();
    let selected_rows = match interact(&term_handle, &rows, username.as_deref()) {
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
            no_template: opts.no_template,
            redetect: opts.redetect,
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

fn render(
    term_handle: &Term,
    rows: &[Row],
    cursor: usize,
    username: Option<&str>,
) -> io::Result<()> {
    let user_token = username.unwrap_or("<user>");
    term_handle.write_line(
        &term::header(&format!(
            "Setting up MCP integration for AI agents for `{user_token}`."
        ))
        .to_string(),
    )?;
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
        // Create `~/.claude/plugins/aimx` with a real aimx-content file
        // so detection reports InstalledWired. Row
        // is selectable (operator can choose to re-wire / overwrite) but
        // defaults to unselected to avoid unnecessary re-installs.
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp.path().join(".claude").join("plugins").join("aimx");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"mcpServers":{"aimx":{}}}"#,
        )
        .unwrap();
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
            no_template: false,
            redetect: false,
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
            out.contains("re-run `aimx agent-setup`"),
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
        // agent-setup` is for and whose home it's writing to. We walk
        // just the `render` body (not the whole file) so this test's
        // own asserts can mention the new strings without
        // self-tripping a "old wording must not appear" check.
        let source = include_str!("agent_tui.rs");
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
        // gets formatted as a backtick-quoted token in the header.
        // The literal `<user>` fallback only appears when the lookup
        // returns None (containers / no passwd entry).
        let source = include_str!("agent_tui.rs");
        assert!(
            source.contains("env.caller_username()"),
            "run_tui must surface the invoking username via AgentEnv"
        );
        assert!(
            source.contains("`{user_token}`"),
            "render must format the username as a backtick-quoted token"
        );
    }

    #[test]
    fn render_hint_sits_below_rows() {
        // The "Space toggles, Enter confirms, q cancels." hint moved
        // from the header band to a line BELOW the rows — right above
        // the cursor where it's most useful. Source-grep both
        // invariants: the hint string is present, and it's emitted
        // AFTER the row loop in `render`.
        let source = include_str!("agent_tui.rs");
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
