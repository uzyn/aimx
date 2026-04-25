//! Semantic color helpers for CLI output.
//!
//! # Palette
//!
//! - [`success`]: green, used for PASS banners and "operation complete" messages
//! - [`error`]:   red + bold, used for fatal errors and the `Error:` prefix on stderr
//! - [`warn`]:    yellow, used for non-fatal warnings (DNS pending, TLS self-signed)
//! - [`info`]:    plain, reserved for informational output (kept uncolored so the
//!   palette stays minimal; wrap if we ever add a cyan accent)
//! - [`header`]:  bold, used for section headers (e.g. `DNS`, `MCP`, `Hooks`)
//! - [`highlight`]: bold, used to emphasise short inline tokens (keys, commands, mailbox names)
//! - [`dim`]:     dimmed, used for secondary hint text ("→ Add: ..." under a FAIL line)
//! - [`accent`]:  copper (`#B9531C`), reserved for the prompt arrow `→` and the
//!   brand wordmark. No other surface uses copper.
//!
//! # Status marks
//!
//! [`success_mark`], [`fail_mark`], [`warn_mark`], and [`prompt_mark`] return the
//! branding-mandated Unicode marks (`✓ ✗ ⚠ →`) on a TTY and the ASCII fallbacks
//! (`[OK]`, `[FAIL]`, `[WARN]`, `[>]`) when color is disabled (piped, redirected,
//! `NO_COLOR=1`, dumb terminal). Always route status output through these so the
//! fallback vocabulary stays consistent across subcommands.
//!
//! # Deprecated: text badges
//!
//! [`pass_badge`], [`fail_badge`], [`warn_badge`], and [`missing_badge`] return
//! the legacy `PASS`/`FAIL`/`WARN`/`MISSING` string tokens. They are preserved
//! as thin wrappers for one minor release and slated for removal in 0.2.
//! New code should use the `*_mark()` helpers instead.
//!
//! # Convention
//!
//! Raw `.green()` / `.red()` / `.yellow()` / `.blue()` / `.bold()` calls outside
//! this module are banned — the CI `cli-colors` job fails if any leak in.
//! Route every user-facing styled string through a helper here so the palette
//! stays consistent and can be audited in one place.
//!
//! # Disabling color
//!
//! The `colored` crate auto-detects `NO_COLOR`, non-TTY pipes, and dumb terminals.
//! The status-mark helpers also switch to their ASCII fallbacks under the same
//! conditions so `aimx ... | cat` emits pipe-friendly plain text with no ANSI
//! escapes and no Unicode marks.

use colored::{ColoredString, Colorize};

/// Copper accent (`#B9531C`), the one brand color. Reserved for the `→` prompt
/// arrow and the brand wordmark per branding §2.1 and §5.4.
#[allow(dead_code)]
pub fn accent(s: &str) -> ColoredString {
    s.truecolor(185, 83, 28)
}

pub fn success(s: &str) -> ColoredString {
    s.green()
}

/// Bold + green, reserved for the top-level "Setup complete" banner.
pub fn success_banner(s: &str) -> ColoredString {
    s.green().bold()
}

pub fn error(s: &str) -> ColoredString {
    s.red().bold()
}

pub fn warn(s: &str) -> ColoredString {
    s.yellow()
}

pub fn info(s: &str) -> ColoredString {
    s.normal()
}

pub fn header(s: &str) -> ColoredString {
    s.bold()
}

pub fn highlight(s: &str) -> ColoredString {
    s.bold()
}

pub fn dim(s: &str) -> ColoredString {
    s.dimmed()
}

/// Returns true when the output stream should carry ANSI escapes (TTY + not
/// disabled by `NO_COLOR`). Matches the `colored` crate's internal decision.
fn colorize_active() -> bool {
    colored::control::SHOULD_COLORIZE.should_colorize()
}

/// Green `✓` on a TTY, `[OK]` on non-TTY / `NO_COLOR`.
pub fn success_mark() -> ColoredString {
    if colorize_active() {
        "✓".green()
    } else {
        "[OK]".normal()
    }
}

/// Red `✗` on a TTY, `[FAIL]` on non-TTY / `NO_COLOR`.
pub fn fail_mark() -> ColoredString {
    if colorize_active() {
        "✗".red()
    } else {
        "[FAIL]".normal()
    }
}

/// Yellow `⚠` on a TTY, `[WARN]` on non-TTY / `NO_COLOR`.
pub fn warn_mark() -> ColoredString {
    if colorize_active() {
        "⚠".yellow()
    } else {
        "[WARN]".normal()
    }
}

/// State of one entry in the setup wizard's six-step checklist. The
/// rendering for each state is owned by [`step_glyph`] so the binary
/// emits a consistent banner across every section transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Skipped,
    /// Reserved for surfaces that surface a fatal step-level error
    /// without aborting the wizard. The current `aimx setup` flow never
    /// reaches this state because hard errors `return Err(...)` and exit
    /// the process; future surfaces (e.g. `aimx upgrade`'s rollback
    /// path) may use it to render `✗` next to the failed step.
    #[allow(dead_code)]
    Error,
}

/// Render the checklist glyph for `state`. Unicode (☐ / ◐ / ☑ / ☒ / ✗)
/// on a TTY with color enabled, ASCII fallback (`[ ]` / `[~]` / `[x]` /
/// `[-]` / `[!]`) when color is disabled (piped, redirected, `NO_COLOR=1`,
/// or dumb terminal). Color: pending=dim, running=yellow, done=green,
/// skipped=cyan, error=red.
pub fn step_glyph(state: StepState) -> ColoredString {
    if colorize_active() {
        match state {
            StepState::Pending => "☐".dimmed(),
            StepState::Running => "◐".yellow(),
            StepState::Done => "☑".green(),
            StepState::Skipped => "☒".cyan(),
            StepState::Error => "✗".red(),
        }
    } else {
        match state {
            StepState::Pending => "[ ]".normal(),
            StepState::Running => "[~]".normal(),
            StepState::Done => "[x]".normal(),
            StepState::Skipped => "[-]".normal(),
            StepState::Error => "[!]".normal(),
        }
    }
}

/// Copper `→` on a TTY, `[>]` on non-TTY / `NO_COLOR`. The `colored` crate
/// emits truecolor escapes on truecolor-capable terminals and transparently
/// falls back to the nearest 256-color ANSI code elsewhere.
pub fn prompt_mark() -> ColoredString {
    if colorize_active() {
        // truecolor used; on 256-color terminals, `colored` falls back to the
        // nearest named color (red). A precise ANSI-208 fallback would require
        // either `Color::Fixed(208)` via a different crate or a manual
        // `COLORTERM` probe; deferred with the owo-colors migration to v2
        // (PRD §9).
        "→".truecolor(185, 83, 28)
    } else {
        "[>]".normal()
    }
}

/// Deprecated: use [`success_mark`] instead. Retained as a thin wrapper for
/// one minor release; slated for removal in 0.2.
#[allow(dead_code)]
#[deprecated(
    since = "0.1.0",
    note = "use `term::success_mark()` instead; will be removed in 0.2.0"
)]
pub fn pass_badge() -> ColoredString {
    "PASS".green()
}

/// Deprecated: use [`fail_mark`] instead. Retained as a thin wrapper for one
/// minor release; slated for removal in 0.2.
#[allow(dead_code)]
#[deprecated(
    since = "0.1.0",
    note = "use `term::fail_mark()` instead; will be removed in 0.2.0"
)]
pub fn fail_badge() -> ColoredString {
    "FAIL".red()
}

/// Deprecated: use [`warn_mark`] instead. Retained as a thin wrapper for one
/// minor release; slated for removal in 0.2.
#[allow(dead_code)]
#[deprecated(
    since = "0.1.0",
    note = "use `term::warn_mark()` instead; will be removed in 0.2.0"
)]
pub fn warn_badge() -> ColoredString {
    "WARN".yellow()
}

/// Deprecated: use [`fail_mark`] instead. Retained as a thin wrapper for one
/// minor release; slated for removal in 0.2.
#[allow(dead_code)]
#[deprecated(
    since = "0.1.0",
    note = "use `term::fail_mark()` instead; will be removed in 0.2.0"
)]
pub fn missing_badge() -> ColoredString {
    "MISSING".red()
}

#[cfg(test)]
mod tests {
    use super::*;
    use colored::control;
    use std::sync::Mutex;

    // `colored::control::set_override` mutates process-global state, so any two
    // tests that toggle it must not run concurrently.
    static COLOR_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn no_color_strips_ansi_from_helpers() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(false);
        let outputs = [
            success("done").to_string(),
            error("bad").to_string(),
            warn("careful").to_string(),
            info("hello").to_string(),
            header("DNS").to_string(),
            highlight("aimx").to_string(),
            dim("hint").to_string(),
            accent("→ next").to_string(),
            success_mark().to_string(),
            fail_mark().to_string(),
            warn_mark().to_string(),
            prompt_mark().to_string(),
        ];
        control::unset_override();
        for s in &outputs {
            assert!(
                !s.contains('\x1b'),
                "expected no ANSI escape in {s:?} when color is disabled"
            );
        }
    }

    #[test]
    fn helpers_produce_ansi_when_color_forced_on() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(true);
        let s = success("ok").to_string();
        let acc = accent("→").to_string();
        let prompt = prompt_mark().to_string();
        control::unset_override();
        assert!(
            s.contains('\x1b'),
            "expected ANSI escape when color is forced on, got {s:?}"
        );
        assert!(
            acc.contains('\x1b'),
            "expected ANSI escape on accent() when color is forced on, got {acc:?}"
        );
        assert!(
            prompt.contains('\x1b'),
            "expected ANSI escape on prompt_mark() when color is forced on, got {prompt:?}"
        );
    }

    #[test]
    fn marks_use_unicode_on_tty() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(true);
        let success_s = success_mark().to_string();
        let fail_s = fail_mark().to_string();
        let warn_s = warn_mark().to_string();
        let prompt_s = prompt_mark().to_string();
        control::unset_override();
        assert!(
            success_s.contains('✓'),
            "expected ✓ in success_mark on TTY, got {success_s:?}"
        );
        assert!(
            fail_s.contains('✗'),
            "expected ✗ in fail_mark on TTY, got {fail_s:?}"
        );
        assert!(
            warn_s.contains('⚠'),
            "expected ⚠ in warn_mark on TTY, got {warn_s:?}"
        );
        assert!(
            prompt_s.contains('→'),
            "expected → in prompt_mark on TTY, got {prompt_s:?}"
        );
    }

    #[test]
    fn marks_use_ascii_fallback_on_non_tty() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(false);
        let success_s = success_mark().to_string();
        let fail_s = fail_mark().to_string();
        let warn_s = warn_mark().to_string();
        let prompt_s = prompt_mark().to_string();
        control::unset_override();
        assert_eq!(success_s, "[OK]");
        assert_eq!(fail_s, "[FAIL]");
        assert_eq!(warn_s, "[WARN]");
        assert_eq!(prompt_s, "[>]");
    }

    #[test]
    fn prompt_mark_encodes_copper_truecolor_on_tty() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: single-threaded inside COLOR_OVERRIDE_LOCK, and we only ever
        // read COLORTERM from the `colored` crate, which does the read via
        // std::env::var — not in a signal handler or from another thread.
        let saved_colorterm = std::env::var("COLORTERM").ok();
        unsafe { std::env::set_var("COLORTERM", "truecolor") };
        control::set_override(true);
        let s = prompt_mark().to_string();
        control::unset_override();
        match saved_colorterm {
            Some(v) => unsafe { std::env::set_var("COLORTERM", v) },
            None => unsafe { std::env::remove_var("COLORTERM") },
        }
        // Truecolor ANSI sequence for #B9531C is `\x1b[38;2;185;83;28m`.
        assert!(
            s.contains("38;2;185;83;28"),
            "expected copper truecolor escape in prompt_mark, got {s:?}"
        );
    }

    #[test]
    fn prompt_mark_degrades_when_colorterm_absent() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_colorterm = std::env::var("COLORTERM").ok();
        // SAFETY: as above.
        unsafe { std::env::remove_var("COLORTERM") };
        control::set_override(true);
        let s = prompt_mark().to_string();
        control::unset_override();
        if let Some(v) = saved_colorterm {
            unsafe { std::env::set_var("COLORTERM", v) };
        }
        // Without truecolor, `colored` picks the nearest named ANSI color. The
        // copper hue is reddish, so that fallback is `\x1b[31m...`. The
        // arrow character itself must survive the degrade.
        assert!(
            s.contains('→'),
            "expected → in prompt_mark even when COLORTERM is absent, got {s:?}"
        );
        assert!(
            s.contains('\x1b'),
            "expected ANSI escape in prompt_mark even when COLORTERM is absent, got {s:?}"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_badges_still_carry_expected_text() {
        assert!(pass_badge().to_string().contains("PASS"));
        assert!(fail_badge().to_string().contains("FAIL"));
        assert!(warn_badge().to_string().contains("WARN"));
        assert!(missing_badge().to_string().contains("MISSING"));
    }

    #[test]
    fn step_glyphs_use_unicode_on_tty() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(true);
        let pending = step_glyph(StepState::Pending).to_string();
        let running = step_glyph(StepState::Running).to_string();
        let done = step_glyph(StepState::Done).to_string();
        let skipped = step_glyph(StepState::Skipped).to_string();
        let error = step_glyph(StepState::Error).to_string();
        control::unset_override();
        assert!(
            pending.contains('☐'),
            "expected ☐ in pending step glyph on TTY, got {pending:?}"
        );
        assert!(
            running.contains('◐'),
            "expected ◐ in running step glyph on TTY, got {running:?}"
        );
        assert!(
            done.contains('☑'),
            "expected ☑ in done step glyph on TTY, got {done:?}"
        );
        assert!(
            skipped.contains('☒'),
            "expected ☒ in skipped step glyph on TTY, got {skipped:?}"
        );
        assert!(
            error.contains('✗'),
            "expected ✗ in error step glyph on TTY, got {error:?}"
        );
    }

    #[test]
    fn step_glyphs_use_ascii_fallback_on_non_tty() {
        let _guard = COLOR_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        control::set_override(false);
        let pending = step_glyph(StepState::Pending).to_string();
        let running = step_glyph(StepState::Running).to_string();
        let done = step_glyph(StepState::Done).to_string();
        let skipped = step_glyph(StepState::Skipped).to_string();
        let error = step_glyph(StepState::Error).to_string();
        control::unset_override();
        assert_eq!(pending, "[ ]");
        assert_eq!(running, "[~]");
        assert_eq!(done, "[x]");
        assert_eq!(skipped, "[-]");
        assert_eq!(error, "[!]");
    }
}
