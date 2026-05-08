//! Markdown → email-ready HTML renderer.
//!
//! Pure transform: a UTF-8 Markdown source becomes a self-contained HTML
//! document with per-element inlined styles, suitable for use as the
//! `text/html` part of a `multipart/alternative` outbound message. No I/O,
//! no remote resources, no `<style>` block — Gmail and Outlook for Web
//! strip those, so every style lives directly on the element it targets.
//!
//! The renderer config (CommonMark + GFM extensions) is built once per
//! process via `OnceLock` so high-volume sends don't repeat option setup.
//! Output is deterministic: the same input bytes produce byte-identical
//! HTML across invocations of the same binary, which is what justifies
//! storing only the Markdown source in `sent/<mailbox>/` (the recipient's
//! HTML view is recoverable by re-running this function).

use std::fmt;
use std::sync::OnceLock;

use comrak::{Options, markdown_to_html};

/// Hard cap on the Markdown source byte length the renderer accepts.
///
/// 5 MiB Markdown renders to ~15-25 MiB on the wire after HTML expansion +
/// base64 transfer-encoding overhead, which sits at the edge of mainstream
/// receiver limits (Gmail 25 MB, Outlook.com / iCloud 20 MB). Anything
/// larger belongs as an attachment, not as a body.
pub const MAX_MARKDOWN_BODY_BYTES: usize = 5 * 1024 * 1024;

/// Errors returned by the renderer entry point.
#[derive(Debug)]
pub enum MarkdownRenderError {
    /// The Markdown source exceeded `MAX_MARKDOWN_BODY_BYTES`.
    BodyTooLarge,
}

impl fmt::Display for MarkdownRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkdownRenderError::BodyTooLarge => f.write_str(
                "markdown body exceeds 5 MiB; use --html-body for pre-rendered large documents or --attachment for sending the document as a file",
            ),
        }
    }
}

impl std::error::Error for MarkdownRenderError {}

fn comrak_options() -> &'static Options<'static> {
    static OPTS: OnceLock<Options<'static>> = OnceLock::new();
    OPTS.get_or_init(|| {
        let mut opts = Options::default();
        // GFM extensions per the PRD.
        opts.extension.table = true;
        opts.extension.strikethrough = true;
        opts.extension.autolink = true;
        opts.extension.tasklist = true;
        opts.extension.footnotes = true;
        opts.extension.tagfilter = true;
        // In-document anchors. Empty prefix keeps generated IDs stable
        // across versions and matches the un-prefixed shape rich-text
        // mail clients expect.
        opts.extension.header_id_prefix = Some(String::new());
        // Raw HTML embedded in Markdown is escaped, never rendered.
        // Operators wanting raw HTML pass through use --html-body.
        opts.render.r#unsafe = false;
        opts
    })
}

/// Test-only counter incremented immediately before each `comrak` call.
/// Lets the body-cap test prove comrak never runs on oversize input.
/// Paired with `COMRAK_PROBE_LOCK` so the observing test can take an
/// exclusive write-lock that blocks any concurrent renderer thread from
/// touching the counter mid-measurement.
#[cfg(test)]
static COMRAK_INVOCATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static COMRAK_PROBE_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Render a Markdown source to email-ready HTML with inlined per-element
/// styles. Returns `BodyTooLarge` when the source exceeds the configured
/// byte cap; comrak is **not** invoked in that case.
pub fn render_markdown_to_email_html(markdown: &str) -> Result<String, MarkdownRenderError> {
    if markdown.len() > MAX_MARKDOWN_BODY_BYTES {
        return Err(MarkdownRenderError::BodyTooLarge);
    }
    #[cfg(test)]
    let _probe = COMRAK_PROBE_LOCK.read().expect("probe lock poisoned");
    #[cfg(test)]
    COMRAK_INVOCATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let raw_html = markdown_to_html(markdown, comrak_options());
    Ok(inline_email_styles(&raw_html))
}

// ---------------------------------------------------------------------------
// Inlined stylesheet
// ---------------------------------------------------------------------------

// Per-element styles, baked in as `&'static str` constants. Values are the
// minimum needed for tasteful rendering across Gmail / Outlook / Apple
// Mail without external resources. Expand only when a user-visible
// element renders unstyled.
const STYLE_BODY: &str = "font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; line-height: 1.55; max-width: 720px; margin: 0 auto; padding: 16px; color: #1f2328;";
const STYLE_H1: &str = "font-size: 1.75em; margin: 1.2em 0 0.5em; line-height: 1.25;";
const STYLE_H2: &str = "font-size: 1.4em; margin: 1.1em 0 0.5em; line-height: 1.3;";
const STYLE_H3: &str = "font-size: 1.2em; margin: 1em 0 0.4em; line-height: 1.3;";
const STYLE_H4: &str = "font-size: 1.05em; margin: 1em 0 0.4em; line-height: 1.3;";
const STYLE_P: &str = "margin: 0.6em 0;";
const STYLE_UL: &str = "margin: 0.6em 0; padding-left: 1.4em;";
const STYLE_OL: &str = "margin: 0.6em 0; padding-left: 1.4em;";
const STYLE_LI: &str = "margin: 0.2em 0;";
const STYLE_A: &str = "color: #0b66c2; text-decoration: none;";
const STYLE_CODE: &str = "font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background: #f3f4f6; padding: 0.1em 0.3em; border-radius: 3px; font-size: 0.95em;";
const STYLE_PRE: &str = "font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background: #f3f4f6; padding: 12px; border-radius: 5px; overflow-x: auto; font-size: 0.95em; line-height: 1.45;";
const STYLE_BLOCKQUOTE: &str =
    "border-left: 3px solid #d0d7de; padding: 0 0.9em; color: #57606a; margin: 0.6em 0;";
const STYLE_TABLE: &str = "border-collapse: collapse; margin: 0.8em 0; border: 1px solid #d0d7de;";
const STYLE_TH: &str =
    "border: 1px solid #d0d7de; padding: 6px 10px; background: #f6f8fa; text-align: left;";
const STYLE_TD: &str = "border: 1px solid #d0d7de; padding: 6px 10px;";
const STYLE_HR: &str = "border: 0; border-top: 1px solid #d0d7de; margin: 1.4em 0;";

const STYLED_TAGS: &[(&str, &str)] = &[
    ("body", STYLE_BODY),
    ("h1", STYLE_H1),
    ("h2", STYLE_H2),
    ("h3", STYLE_H3),
    ("h4", STYLE_H4),
    ("p", STYLE_P),
    ("ul", STYLE_UL),
    ("ol", STYLE_OL),
    ("li", STYLE_LI),
    ("a", STYLE_A),
    ("code", STYLE_CODE),
    ("pre", STYLE_PRE),
    ("blockquote", STYLE_BLOCKQUOTE),
    ("table", STYLE_TABLE),
    ("th", STYLE_TH),
    ("td", STYLE_TD),
    ("hr", STYLE_HR),
];

/// Rewrite `html` so each supported tag carries an inline `style="..."`
/// attribute. Existing `style` attributes on the same tag are preserved
/// (the renderer's defaults are appended to the end so inline overrides
/// keep precedence). Unsupported tags are left untouched.
pub fn inline_email_styles(html: &str) -> String {
    use lol_html::{HtmlRewriter, Settings, element};

    let mut output: Vec<u8> = Vec::with_capacity(html.len() + 1024);
    let element_handlers: Vec<_> = STYLED_TAGS
        .iter()
        .map(|(tag, style)| {
            element!(*tag, move |el| {
                let merged = match el.get_attribute("style") {
                    Some(existing) if !existing.trim().is_empty() => {
                        let trimmed = existing.trim_end_matches(';').trim_end();
                        format!("{trimmed}; {style}")
                    }
                    _ => (*style).to_string(),
                };
                el.set_attribute("style", &merged)
                    .expect("style is always a valid attribute name");
                Ok(())
            })
        })
        .collect();

    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: element_handlers,
            ..Settings::default()
        },
        |chunk: &[u8]| output.extend_from_slice(chunk),
    );

    if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
        // The rewriter only fails on internal-state issues we don't expect
        // for our well-formed comrak output; fall back to the un-rewritten
        // HTML so a render still produces a usable message.
        return html.to_string();
    }

    String::from_utf8(output).unwrap_or_else(|_| {
        // Unreachable today: input is comrak-produced HTML which is always
        // valid UTF-8, and lol_html only emits the bytes the handlers
        // produce (also UTF-8 here). Surface a future regression in debug
        // builds; release builds degrade gracefully to un-styled HTML.
        debug_assert!(
            false,
            "rewriter produced invalid UTF-8 — silent degrade to un-styled HTML",
        );
        html.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- comrak config + GFM extensions ----

    #[test]
    fn renders_heading_and_unordered_list() {
        let out = render_markdown_to_email_html("# Hello\n\n- a\n- b\n").unwrap();
        assert!(out.contains("<h1"), "missing <h1 tag in {out}");
        assert!(out.contains(">Hello</h1>"), "missing heading text in {out}");
        assert!(out.contains("<ul"), "missing <ul tag in {out}");
        // Two list items.
        assert_eq!(out.matches("<li").count(), 2, "expected 2 <li in {out}");
    }

    #[test]
    fn raw_html_in_markdown_is_neutralised() {
        // Raw HTML embedded in Markdown must never reach the recipient
        // as an executable element. With `unsafe = false` plus the GFM
        // tagfilter, comrak strips disallowed tags (`<script>`,
        // `<iframe>`, …) entirely; for arbitrary raw HTML it emits the
        // input as a stripped / escaped form. Both shapes satisfy the
        // security invariant — assert that, not the exact rendering.
        let out = render_markdown_to_email_html("<script>alert(1)</script>").unwrap();
        assert!(
            !out.contains("<script>"),
            "raw <script> tag must not survive: {out}",
        );
        assert!(
            !out.contains("alert(1)"),
            "script body must not survive into output: {out}",
        );

        // A non-tagfiltered raw element (`<b>`) should not be parsed as
        // HTML either — the inner text appears, but the angle brackets
        // are escaped so the recipient sees the literal source.
        let out2 = render_markdown_to_email_html("<b>bold</b>").unwrap();
        assert!(
            !out2.contains("<b>bold</b>"),
            "raw <b> must not be rendered as HTML: {out2}",
        );
        assert!(
            out2.contains("&lt;b&gt;") || out2.contains("<!-- raw HTML omitted -->"),
            "expected escaped or stripped raw HTML: {out2}",
        );
    }

    #[test]
    fn gfm_table_renders_with_table_thead_tbody() {
        let md = "| h1 | h2 |\n|----|----|\n| a  | b  |\n| c  | d  |\n";
        let out = render_markdown_to_email_html(md).unwrap();
        assert!(out.contains("<table"), "missing <table in {out}");
        assert!(out.contains("<thead"), "missing <thead in {out}");
        assert!(out.contains("<tbody"), "missing <tbody in {out}");
        assert!(out.contains("<th"), "missing <th in {out}");
        assert!(out.contains("<td"), "missing <td in {out}");
    }

    #[test]
    fn comrak_options_initialise_only_once() {
        // Both calls return the same pointer, proving the OnceLock is wired.
        let a = comrak_options() as *const _;
        let b = comrak_options() as *const _;
        assert_eq!(a, b, "comrak options should be a process-wide singleton");
    }

    // ---- inlined stylesheet ----

    #[test]
    fn h1_carries_inline_style_attribute() {
        let out = render_markdown_to_email_html("# Title").unwrap();
        assert!(
            out.contains("<h1 style=\""),
            "expected <h1 style=\"...\" in {out}",
        );
    }

    #[test]
    fn no_external_resources_in_output() {
        let out = render_markdown_to_email_html("# T\n\nbody\n").unwrap();
        assert!(!out.contains("<style"), "no embedded <style> block: {out}");
        assert!(!out.contains("<link"), "no <link> elements: {out}");
        assert!(
            !out.to_lowercase().contains("http://"),
            "no remote resources injected by renderer: {out}",
        );
        // The comrak output should not reach for fonts.googleapis or any
        // CDN — our styles are baked-in only.
        assert!(!out.contains("googleapis"), "no Google Fonts: {out}");
    }

    #[test]
    fn autolinked_url_is_anchor_with_inline_style() {
        let out = render_markdown_to_email_html("Visit https://example.com please.\n").unwrap();
        assert!(out.contains("<a"), "expected <a element in {out}");
        // The autolinked anchor must carry our inline style.
        let a_idx = out.find("<a").expect("anchor exists");
        let after_a = &out[a_idx..a_idx + 200.min(out.len() - a_idx)];
        assert!(
            after_a.contains("style=\""),
            "anchor missing inline style: {after_a}",
        );
        // No underline at rest — the default style omits text-decoration.
        assert!(
            !after_a.contains("text-decoration: underline"),
            "anchor should not be underlined at rest: {after_a}",
        );
    }

    #[test]
    fn nested_pre_code_keeps_both_styles() {
        let md = "```\nlet x = 1;\n```\n";
        let out = render_markdown_to_email_html(md).unwrap();
        // The rewriter should style both the outer <pre> and the inner <code>.
        let pre_idx = out.find("<pre").expect("expected <pre");
        let code_idx = out.find("<code").expect("expected <code");
        assert!(
            out[pre_idx..pre_idx + 200.min(out.len() - pre_idx)].contains("style=\""),
            "<pre> missing style: {out}",
        );
        assert!(
            out[code_idx..code_idx + 200.min(out.len() - code_idx)].contains("style=\""),
            "<code> missing style: {out}",
        );
    }

    #[test]
    fn inline_styles_preserve_existing_style_attribute() {
        // Synthetic input: an <h1> with a pre-existing style attribute.
        // The renderer-defaults must be appended without dropping the
        // existing declaration.
        let html = "<h1 style=\"color: red\">Hello</h1>";
        let out = inline_email_styles(html);
        assert!(out.contains("color: red"), "existing style dropped: {out}");
        assert!(out.contains("font-size"), "default style missing: {out}");
    }

    // ---- 5 MiB body cap ----

    #[test]
    fn body_at_exact_cap_succeeds() {
        // Exactly MAX bytes. Use a single-character ASCII so byte length
        // equals char count.
        let body = "a".repeat(MAX_MARKDOWN_BODY_BYTES);
        let result = render_markdown_to_email_html(&body);
        assert!(
            result.is_ok(),
            "body of exactly the cap should render, got {:?}",
            result.err(),
        );
    }

    #[test]
    fn body_one_byte_over_cap_is_rejected_with_canonical_message() {
        let body = "a".repeat(MAX_MARKDOWN_BODY_BYTES + 1);
        let err = render_markdown_to_email_html(&body).expect_err("expected BodyTooLarge");
        assert!(matches!(err, MarkdownRenderError::BodyTooLarge));
        assert_eq!(
            err.to_string(),
            "markdown body exceeds 5 MiB; use --html-body for pre-rendered large documents or --attachment for sending the document as a file",
        );
    }

    #[test]
    fn oversize_body_does_not_invoke_comrak() {
        // The renderer increments `COMRAK_INVOCATIONS` immediately before
        // each comrak call (test-only) under a shared read-lock on
        // `COMRAK_PROBE_LOCK`. Taking the write-lock here blocks every
        // other concurrent renderer thread from touching the counter for
        // the duration of the measurement, so a stable snapshot proves
        // the early-return ran before comrak — not just that the error
        // type matches.
        use std::sync::atomic::Ordering;
        let _probe = COMRAK_PROBE_LOCK.write().expect("probe lock poisoned");

        let before = COMRAK_INVOCATIONS.load(Ordering::Relaxed);
        let body = ">".repeat(MAX_MARKDOWN_BODY_BYTES + 1);
        let err = render_markdown_to_email_html(&body).expect_err("expected BodyTooLarge");
        let after_oversize = COMRAK_INVOCATIONS.load(Ordering::Relaxed);

        assert!(matches!(err, MarkdownRenderError::BodyTooLarge));
        assert_eq!(
            before, after_oversize,
            "comrak must not be invoked for an oversize body (before={before}, after={after_oversize})",
        );

        // Sanity check: an under-cap call DOES tick the counter under the
        // same write-lock window, so the counter is wired correctly and
        // the equality above is meaningful rather than vacuous. The
        // renderer's own read-lock acquisition is reentrant-safe here
        // because `RwLock::read` from the writer thread on a write-held
        // lock would deadlock — so we drop the write-lock first, take
        // the read-lock implicitly via the renderer call, and re-take
        // the write-lock after to read the counter.
        drop(_probe);
        let pre_tick = COMRAK_INVOCATIONS.load(Ordering::Relaxed);
        let _ = render_markdown_to_email_html("# tick\n").unwrap();
        let _probe2 = COMRAK_PROBE_LOCK.write().expect("probe lock poisoned");
        let post_tick = COMRAK_INVOCATIONS.load(Ordering::Relaxed);
        assert!(
            post_tick > pre_tick,
            "under-cap render should tick the counter (pre_tick={pre_tick}, post_tick={post_tick})",
        );
    }

    #[test]
    fn deterministic_output_within_one_process() {
        let md = "# Hi\n\n- a\n- b\n\n[link](https://example.com)\n";
        let a = render_markdown_to_email_html(md).unwrap();
        let b = render_markdown_to_email_html(md).unwrap();
        assert_eq!(a, b, "renderer must be deterministic in-process");
    }

    // ---- fixture-backed determinism + drift + perf ----

    const BRIEFING_5KB: &str = include_str!("../tests/fixtures/markdown/briefing-5kb.md");
    const BRIEFING_5KB_EXPECTED: &str =
        include_str!("../tests/fixtures/markdown/briefing-5kb.expected.html");
    const REPORT_50KB: &str = include_str!("../tests/fixtures/markdown/report-50kb.md");
    const REPORT_50KB_EXPECTED: &str =
        include_str!("../tests/fixtures/markdown/report-50kb.expected.html");

    /// On drift, the assertion message must point operators at the
    /// `*.expected.html` files to update **and** prompt for a release-
    /// notes line. Computed once and reused by both fixture tests.
    fn drift_help(label: &str) -> String {
        format!(
            "renderer output drifted from the checked-in expected HTML for {label}.\n\
             Update tests/fixtures/markdown/{label}.expected.html with the new bytes \
             AND add a release-notes line documenting the renderer-version change \
             (recipient HTML view of every previously-sent message will differ).",
        )
    }

    /// When `AIMX_BLESS_EXPECTED=1` is set, write the fresh renderer
    /// output back to the on-disk expected fixture instead of asserting.
    /// Lets a maintainer re-bless the fixtures after a deliberate
    /// renderer-version bump in one `cargo test` invocation. The file
    /// path is resolved relative to `CARGO_MANIFEST_DIR` so the bless
    /// step works regardless of cwd.
    fn maybe_bless(label: &str, fresh: &str) {
        if std::env::var("AIMX_BLESS_EXPECTED").ok().as_deref() == Some("1") {
            let manifest = env!("CARGO_MANIFEST_DIR");
            let path = format!("{manifest}/tests/fixtures/markdown/{label}.expected.html");
            std::fs::write(&path, fresh).unwrap_or_else(|e| panic!("failed to bless {path}: {e}"));
        }
    }

    #[test]
    fn briefing_fixture_matches_expected_html() {
        let out = render_markdown_to_email_html(BRIEFING_5KB).unwrap();
        maybe_bless("briefing-5kb", &out);
        if std::env::var("AIMX_BLESS_EXPECTED").ok().as_deref() == Some("1") {
            return;
        }
        assert_eq!(out, BRIEFING_5KB_EXPECTED, "{}", drift_help("briefing-5kb"));
    }

    #[test]
    fn report_fixture_matches_expected_html() {
        let out = render_markdown_to_email_html(REPORT_50KB).unwrap();
        maybe_bless("report-50kb", &out);
        if std::env::var("AIMX_BLESS_EXPECTED").ok().as_deref() == Some("1") {
            return;
        }
        assert_eq!(out, REPORT_50KB_EXPECTED, "{}", drift_help("report-50kb"));
    }

    #[test]
    fn briefing_fixture_renders_deterministically_within_process() {
        let a = render_markdown_to_email_html(BRIEFING_5KB).unwrap();
        let b = render_markdown_to_email_html(BRIEFING_5KB).unwrap();
        assert_eq!(a, b, "briefing render must be byte-stable in-process");
    }

    #[test]
    fn report_fixture_renders_deterministically_within_process() {
        let a = render_markdown_to_email_html(REPORT_50KB).unwrap();
        let b = render_markdown_to_email_html(REPORT_50KB).unwrap();
        assert_eq!(a, b, "report render must be byte-stable in-process");
    }

    #[test]
    fn briefing_rendered_html_under_25kb() {
        let out = render_markdown_to_email_html(BRIEFING_5KB).unwrap();
        assert!(
            out.len() <= 25 * 1024,
            "rendered briefing exceeded 25KB: {} bytes",
            out.len(),
        );
    }

    /// Perf bound — only meaningful in release builds. Debug runs the
    /// renderer ~10x slower and would produce false negatives.
    #[test]
    #[cfg(not(debug_assertions))]
    fn render_perf_5kb_under_10ms() {
        // Warm comrak's OnceLock so the bound measures pure render
        // time, not the one-shot config init.
        let _ = render_markdown_to_email_html("# warmup\n").unwrap();
        let start = std::time::Instant::now();
        let _ = render_markdown_to_email_html(BRIEFING_5KB).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 10,
            "5KB render took {}ms (budget: <10ms)",
            elapsed.as_millis(),
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn render_perf_50kb_under_50ms() {
        let _ = render_markdown_to_email_html("# warmup\n").unwrap();
        let start = std::time::Instant::now();
        let _ = render_markdown_to_email_html(REPORT_50KB).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 50,
            "50KB render took {}ms (budget: <50ms)",
            elapsed.as_millis(),
        );
    }
}
