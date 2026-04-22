//! Built-in hook templates bundled into the aimx binary.
//!
//! The canonical TOML source lives at `hook-templates/defaults.toml` at
//! the repo root so operators and reviewers can read the plain-TOML
//! definitions without digging through Rust struct literals. The file
//! is embedded via `include_str!` and parsed on demand.
//!
//! Sprint 3 (S3-3) retired the interactive setup checkbox that used to
//! drive template selection off this module. Sprint 8 will strip the
//! `invoke-*` blocks from the embedded TOML entirely. For now the
//! module still compile-time-validates the bundled file so a malformed
//! edit is caught at build time, but nothing in the runtime setup
//! flow reads the parsed templates.
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
            8,
            "expected 8 default templates per PRD §6.2, got {}",
            templates.len()
        );
    }

    /// PRD §6.2 pins the exact argv shape and stdin mode for each
    /// default template. This test is a forcing function: drift in
    /// `defaults.toml` must be an explicit update to the PRD, not a
    /// silent change in the shipped binary.
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

        let invoke_claude = by_name("invoke-claude");
        assert_eq!(
            invoke_claude.cmd,
            vec!["/usr/local/bin/claude", "-p", "{prompt}"],
        );
        assert_eq!(invoke_claude.params, vec!["prompt"]);
        assert_eq!(invoke_claude.stdin, HookTemplateStdin::Email);

        let invoke_codex = by_name("invoke-codex");
        assert_eq!(
            invoke_codex.cmd,
            vec!["/usr/local/bin/codex", "-p", "{prompt}"],
        );
        assert_eq!(invoke_codex.params, vec!["prompt"]);
        assert_eq!(invoke_codex.stdin, HookTemplateStdin::Email);

        let invoke_opencode = by_name("invoke-opencode");
        assert_eq!(
            invoke_opencode.cmd,
            vec!["/usr/local/bin/opencode", "run", "{prompt}"],
        );
        assert_eq!(invoke_opencode.params, vec!["prompt"]);
        assert_eq!(invoke_opencode.stdin, HookTemplateStdin::Email);

        let invoke_gemini = by_name("invoke-gemini");
        assert_eq!(
            invoke_gemini.cmd,
            vec!["/usr/local/bin/gemini", "-p", "{prompt}"],
        );
        assert_eq!(invoke_gemini.params, vec!["prompt"]);
        assert_eq!(invoke_gemini.stdin, HookTemplateStdin::Email);

        let invoke_goose = by_name("invoke-goose");
        assert_eq!(
            invoke_goose.cmd,
            vec!["/usr/local/bin/goose", "run", "--recipe", "{recipe}"],
        );
        assert_eq!(invoke_goose.params, vec!["recipe"]);
        // PRD §11 Q3: shipping with `stdin = "email"`; revisit if Goose
        // recipes prove to need a `email_yaml` encoding.
        assert_eq!(invoke_goose.stdin, HookTemplateStdin::Email);

        let invoke_openclaw = by_name("invoke-openclaw");
        assert_eq!(
            invoke_openclaw.cmd,
            vec!["/usr/local/bin/openclaw", "run", "{prompt}"],
        );
        assert_eq!(invoke_openclaw.params, vec!["prompt"]);
        assert_eq!(invoke_openclaw.stdin, HookTemplateStdin::Email);

        let invoke_hermes = by_name("invoke-hermes");
        assert_eq!(
            invoke_hermes.cmd,
            vec!["/usr/local/bin/hermes", "run", "{prompt}"],
        );
        assert_eq!(invoke_hermes.params, vec!["prompt"]);
        assert_eq!(invoke_hermes.stdin, HookTemplateStdin::Email);

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

    /// Every default template runs as `aimx-hook` (never root) and
    /// allows both events unless an explicit override is added later.
    /// This test pins the defaults so a sloppy edit that sets
    /// `run_as = "root"` on a default template fails the suite loudly.
    #[test]
    fn default_templates_all_unprivileged_and_dual_event() {
        for t in default_templates() {
            assert_eq!(
                t.run_as, "aimx-hook",
                "default template '{}' must run as aimx-hook, got '{}'",
                t.name, t.run_as,
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
            vec![
                "invoke-claude",
                "invoke-codex",
                "invoke-opencode",
                "invoke-gemini",
                "invoke-goose",
                "invoke-openclaw",
                "invoke-hermes",
                "webhook",
            ],
            "default template ordering must match PRD §6.3 checkbox list",
        );
    }
}
