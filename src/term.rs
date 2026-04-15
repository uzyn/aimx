//! Semantic color helpers for CLI output.
//!
//! # Palette
//!
//! - [`success`] — green, used for PASS banners and "operation complete" messages
//! - [`error`]   — red + bold, used for fatal errors and the `Error:` prefix on stderr
//! - [`warn`]    — yellow, used for non-fatal warnings (DNS pending, TLS self-signed)
//! - [`info`]    — plain, reserved for informational output (kept uncolored so the
//!   palette stays minimal; wrap if we ever add a cyan accent)
//! - [`header`]  — bold, used for section headers like `[DNS]`, `[MCP]`, `[Deliverability]`
//! - [`highlight`] — bold, used to emphasise short inline tokens (keys, commands, mailbox names)
//! - [`dim`]     — dimmed, used for secondary hint text ("→ Add: ..." under a FAIL line)
//!
//! # Badges
//!
//! [`pass_badge`], [`fail_badge`], and [`warn_badge`] return the short colored
//! `PASS`/`FAIL`/`WARN` tokens rendered inline in check output.
//!
//! # Convention
//!
//! Raw `.green()` / `.red()` / `.yellow()` / `.blue()` / `.bold()` calls outside
//! this module are discouraged — route every user-facing styled string through a
//! helper here so the palette stays consistent and can be audited in one place.
//! A grep for those methods across `src/` should match only this file.
//!
//! # Disabling color
//!
//! The `colored` crate auto-detects `NO_COLOR`, non-TTY pipes, and dumb terminals,
//! so no extra handling is required — piping to `cat` or setting `NO_COLOR=1`
//! strips all ANSI escapes automatically.

use colored::{ColoredString, Colorize};

pub fn success(s: &str) -> ColoredString {
    s.green()
}

/// Bold + green — reserved for the top-level "Setup complete!" banner.
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

pub fn pass_badge() -> ColoredString {
    "PASS".green()
}

pub fn fail_badge() -> ColoredString {
    "FAIL".red()
}

pub fn warn_badge() -> ColoredString {
    "WARN".yellow()
}

pub fn missing_badge() -> ColoredString {
    "MISSING".red()
}

#[cfg(test)]
mod tests {
    use super::*;
    use colored::control;

    fn strip_ansi_test<F: FnOnce()>(f: F) {
        // Force color off for this test regardless of TTY / NO_COLOR state.
        control::set_override(false);
        f();
        control::unset_override();
    }

    #[test]
    fn no_color_strips_ansi_from_helpers() {
        strip_ansi_test(|| {
            let outputs = [
                success("done").to_string(),
                error("bad").to_string(),
                warn("careful").to_string(),
                info("hello").to_string(),
                header("[DNS]").to_string(),
                highlight("aimx").to_string(),
                dim("hint").to_string(),
                pass_badge().to_string(),
                fail_badge().to_string(),
                warn_badge().to_string(),
            ];
            for s in &outputs {
                assert!(
                    !s.contains('\x1b'),
                    "expected no ANSI escape in {s:?} when color is disabled"
                );
            }
        });
    }

    #[test]
    fn helpers_produce_ansi_when_color_forced_on() {
        control::set_override(true);
        let s = success("ok").to_string();
        control::unset_override();
        assert!(
            s.contains('\x1b'),
            "expected ANSI escape when color is forced on, got {s:?}"
        );
    }

    #[test]
    fn badges_carry_expected_text() {
        assert!(pass_badge().to_string().contains("PASS"));
        assert!(fail_badge().to_string().contains("FAIL"));
        assert!(warn_badge().to_string().contains("WARN"));
        assert!(missing_badge().to_string().contains("MISSING"));
    }
}
