//! Built-in hook templates bundled into the aimx binary.
//!
//! The canonical TOML source lives at `hook-templates/defaults.toml` at
//! the repo root so operators and reviewers can read the plain-TOML
//! definitions without digging through Rust struct literals. The file
//! is embedded via `include_str!` and parsed on demand.
//!
//! Sprint 8 (S8-1) stripped every `invoke-<agent>` block from the
//! bundled TOML — per-agent templates are now created on demand by
//! `aimx agent-setup` so they bind to the caller's `$PATH` and uid
//! rather than a hardcoded path. Only the agent-neutral `webhook`
//! template remains pre-bundled. The module still compile-time-
//! validates the bundled file so a malformed edit is caught at
//! build time.
#![allow(dead_code)]

use crate::config::{HookTemplate, validate_hook_templates};
use serde::Deserialize;

/// Raw TOML source, embedded at compile time.
const DEFAULTS_TOML: &str = include_str!("../hook-templates/defaults.toml");

#[derive(Deserialize)]
struct DefaultsFile {
    #[serde(default, rename = "hook_template")]
    hook_templates: Vec<HookTemplate>,
}

/// Parse and return the bundled default templates.
///
/// The returned set is validated via
/// [`crate::config::validate_hook_templates`] so any edit that would
/// break config load is caught at the call site instead of sneaking
/// into a released binary.
///
/// Panics if the embedded TOML is malformed or fails validation —
/// both are compile-time contracts enforced by the
/// `default_templates_load_cleanly` unit test.
pub fn default_templates() -> Vec<HookTemplate> {
    let parsed: DefaultsFile = toml::from_str(DEFAULTS_TOML)
        .expect("embedded hook-templates/defaults.toml must be valid TOML");
    validate_hook_templates(&parsed.hook_templates)
        .expect("embedded default hook templates must pass validate_hook_templates");
    parsed.hook_templates
}

/// Names of all default templates, in the order they appear in the
/// embedded TOML. Retained as a test helper; see the module-level note
/// on Sprint 3 retiring the interactive consumer of this list.
#[cfg(test)]
fn default_template_names() -> Vec<String> {
    default_templates().into_iter().map(|t| t.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookTemplateStdin;
    use crate::hook::HookEvent;

    /// Compile-time smoke test: the embedded TOML parses and passes
    /// full validation. Any future edit to `hook-templates/defaults.toml`
    /// that breaks the schema will fail this test before it reaches CI.
    #[test]
    fn default_templates_load_cleanly() {
        let templates = default_templates();
        assert_eq!(
            templates.len(),
            1,
            "expected 1 default template (webhook) per PRD §6.7, got {}",
            templates.len()
        );
    }

    /// PRD §6.7 pins the exact argv shape and stdin mode for the
    /// `webhook` template — the only default template bundled after
    /// Sprint 8 stripped the per-agent `invoke-*` blocks. This test is
    /// a forcing function: drift in `defaults.toml` must be an explicit
    /// update to the PRD, not a silent change in the shipped binary.
    #[test]
    fn default_templates_match_prd_spec() {
        let templates = default_templates();

        let by_name = |n: &str| -> HookTemplate {
            templates
                .iter()
                .find(|t| t.name == n)
                .cloned()
                .unwrap_or_else(|| panic!("template '{n}' missing from defaults"))
        };

        let webhook = by_name("webhook");
        assert_eq!(
            webhook.cmd,
            vec![
                "/usr/bin/curl",
                "-sS",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                "@-",
                "{url}",
            ],
        );
        assert_eq!(webhook.params, vec!["url"]);
        assert_eq!(webhook.stdin, HookTemplateStdin::EmailJson);
    }

    /// The bundled `webhook` template must never run as root and must
    /// allow both events so operators can bind it to `on_receive` or
    /// `after_send` without further tweaks. `aimx-catchall` is the
    /// safe default `run_as`: the reserved catchall service user is
    /// created on demand by `aimx setup` when the operator configures
    /// a catchall mailbox.
    #[test]
    fn default_templates_all_unprivileged_and_dual_event() {
        for t in default_templates() {
            assert_ne!(
                t.run_as, "root",
                "default template '{}' must not run as root",
                t.name,
            );
            assert_eq!(
                t.timeout_secs, 60,
                "default template '{}' must have 60s timeout, got {}",
                t.name, t.timeout_secs,
            );
            assert!(
                t.allowed_events.contains(&HookEvent::OnReceive)
                    && t.allowed_events.contains(&HookEvent::AfterSend),
                "default template '{}' must allow both events, got {:?}",
                t.name,
                t.allowed_events,
            );
        }
    }

    #[test]
    fn default_template_names_are_unique_and_ordered() {
        let names = default_template_names();
        assert_eq!(
            names,
            vec!["webhook"],
            "default template ordering must match PRD §6.7 (webhook only after Sprint 8)",
        );
    }

    /// Regression guard for Sprint 8 S8-1: no `invoke-*` blocks should
    /// ever re-enter the bundled defaults. Per-agent templates belong
    /// to `aimx agent-setup`, which registers them on demand.
    #[test]
    fn defaults_toml_has_no_invoke_templates() {
        for name in default_template_names() {
            assert!(
                !name.starts_with("invoke-"),
                "bundled default '{name}' must not begin with 'invoke-' (PRD §6.7)",
            );
        }
    }
}
