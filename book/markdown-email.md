# Markdown Email

AIMX is Markdown-native end-to-end. Inbound mail is stored as Markdown for direct LLM consumption; outbound `--body` is interpreted as Markdown by default and rendered to HTML on the wire. Agents read Markdown, write Markdown, and the binary handles the MIME plumbing.

## How a default send is delivered

1. **Caller submits Markdown.** `aimx send --body "..."` (or the MCP `email_send` tool with `body: "..."`) ships the Markdown source to the daemon over the local UDS.
2. **Daemon appends the per-mailbox signature** to the Markdown body so the signature renders as part of the HTML output. Markdown link syntax in the signature renders as a clickable HTML anchor.
3. **Daemon renders Markdown to HTML.** The renderer uses [`comrak`](https://github.com/kivikakk/comrak) configured for CommonMark + GFM extensions (tables, strikethrough, autolinks, task lists, footnotes, tag filter for unsafe tags). Raw HTML embedded in Markdown is escaped, not rendered — operators wanting raw HTML use `--html-body` instead.
4. **Inlined stylesheet pass.** The renderer walks the rendered HTML tree and adds `style="..."` attributes per element. Inlining is required because Gmail, Outlook for Web, and Yahoo Mail strip or limit `<style>` blocks.
5. **Multipart assembly.** The daemon builds a `multipart/alternative` MIME message with the **Markdown source** as the `text/plain` part and the **rendered HTML** as the `text/html` part. With one or more attachments, the `multipart/alternative` is wrapped in an outer `multipart/mixed` so attachments sit as siblings of the alternative block.
6. **DKIM signing.** The signed body bytes are the canonical message body — the new MIME shape doesn't affect the signing logic, and DKIM verification works identically across plain-text, Markdown-rendered, and `--html-body` paths.
7. **MX delivery.** The daemon resolves the recipient domain via MX records and delivers the signed message.

The recipient sees:

- **Rich-text clients (Gmail, Outlook, Apple Mail, Thunderbird):** the rendered HTML with headings, links, tables, blockquotes, code blocks — all styled by the inlined stylesheet.
- **Text-only clients (`mutt`, `mailx`, screen readers in plain mode):** the Markdown source verbatim. Markdown is good plain text by design — `# Heading` is clear, `[link](url)` shows the URL inline, bullets with `-` look like bullets.

## Supported Markdown features

The default render path supports CommonMark plus GFM extensions:

| Feature | Markdown input | Renders as |
|---------|----------------|------------|
| Headings | `# H1` … `#### H4` | `<h1>` … `<h4>` with sized typography |
| Paragraphs | blank-line separated | `<p>` with comfortable line-height |
| Bold / italic | `**bold**`, `*italic*` | `<strong>`, `<em>` |
| Strikethrough (GFM) | `~~strike~~` | `<del>` |
| Links | `[text](https://url)` | `<a>` with non-default link color, no underline at rest |
| Autolinks (GFM) | bare `https://example.com` | `<a>` |
| Inline code | `` `code` `` | `<code>` with monospace + light gray background |
| Code blocks | ` ```lang … ``` ` | `<pre><code>` with monospace + padded background |
| Blockquotes | `> quoted` | `<blockquote>` with left border |
| Horizontal rule | `---` | `<hr>` thin neutral rule |
| Unordered list | `- item` | `<ul><li>` with sensible spacing |
| Ordered list | `1. item` | `<ol><li>` with sensible spacing |
| Tables (GFM) | `\| h1 \| h2 \|` / `\|---\|---\|` / cells | `<table>` with collapsed borders and subtle row separators |
| Task lists (GFM) | `- [x] done` / `- [ ] todo` | checkbox-prefixed list items |
| Footnotes (GFM) | `text[^1]` … `[^1]: note` | numbered footnote anchor + reference list |
| Tag filter (GFM) | `<script>...</script>` | stripped (security) |

## Built-in stylesheet

The inlined stylesheet covers the elements above. It is intentionally minimal — operators wanting custom CSS use `--html-body` for now.

| Element | Style intent |
|---------|--------------|
| `body` | sans-serif font stack, `line-height: 1.55`, `max-width: 720px`, comfortable padding |
| `h1`, `h2`, `h3`, `h4` | size scale, modest top margin |
| `p`, `ul`, `ol`, `li` | sensible spacing |
| `a` | non-default link color (avoids the browser-default purple-after-visit), no underline at rest |
| `code`, `pre` | monospace, light gray background, padding |
| `blockquote` | left border, dim foreground |
| `table`, `th`, `td` | collapsed borders, subtle row separators |
| `hr` | thin neutral rule |

The stylesheet is self-contained: no `<link>` to Google Fonts, no remote CSS, no remote images injected by the renderer. Privacy-conscious clients and corporate firewalls do not strip the rendering. The total rendered HTML for a typical 5KB Markdown briefing fits comfortably under 25KB.

If your content uses an HTML element the inlined stylesheet does not cover (e.g. `<details>`, `<sub>`), it renders unstyled (browser defaults). The element list above is the v1 scope; expand on real demand.

## Body size limit

The renderer enforces a **5 MiB cap** on the Markdown source byte length (the constant is `MAX_MARKDOWN_BODY_BYTES = 5 * 1024 * 1024`). Above the cap, the daemon refuses with the canonical error:

```
markdown body exceeds 5 MiB; use --html-body for pre-rendered large documents or --attachment for sending the document as a file
```

Rationale: a 5 MiB Markdown body renders to roughly 15–25 MiB on the wire (Markdown source + HTML + inlined styles, with ~37% base64 overhead). That sits at the edge of mainstream receiver caps — Gmail 25 MB, Outlook.com / iCloud 20 MB, Microsoft 365 up to 150 MB. Send anything larger as an attachment, not as a body.

The cap is enforced at the renderer entry point so all callers (`aimx send`, MCP `email_send`, future cron jobs) share one limit. Operators on the wire surface that scripts can branch on the failure see the dedicated `ERR BODY_TOO_LARGE` ack code; the canonical reason string survives in the wire response for human readers.

## Escape hatches

### `--text-only`

Forces the wire to single-part `text/plain` with `--body` shipped verbatim. No Markdown rendering, no HTML part, no `multipart/alternative` wrapper.

```bash
aimx send --from alice@example.com --to bob@example.com \
  --subject "Verification code" \
  --body "Your code: 184293" \
  --text-only
```

The per-mailbox signature is **not** auto-appended on this path — the operator already chose plain-text shape; the binary stays out of the way.

Use for:
- OTPs, transactional one-liners, system-generated alerts.
- Migrating existing scripts that depended on `--body` shipping `text/plain`. Adding `--text-only` preserves the old shape exactly.

### `--html-body`

Supplies a custom HTML body. AIMX uses the `--html-body` value verbatim as the `text/html` part and uses `--body` as the `text/plain` fallback so text-only clients still see something readable.

```bash
aimx send --from alice@example.com --to bob@example.com \
  --subject "Newsletter" \
  --body "Plain-text fallback for text-only clients." \
  --html-body "$(cat newsletter.html)"
```

The shell pattern `--html-body "$(cat template.html)"` reads the template from a file and passes it on the command line. Linux's typical `ARG_MAX` is around 1MB, well above any reasonable HTML email size. For templates that exceed `ARG_MAX`, write the template to a tempfile and use a wrapper script — but at that size you should consider attaching the document instead.

`--html-body` and `--text-only` are mutually exclusive. clap rejects the invocation before any UDS round-trip. The same canonical error fires server-side on the MCP `email_send` / `email_reply` tools so operators see one consistent message regardless of the surface.

The per-mailbox signature is **not** auto-appended on this path — operator-supplied content is verbatim. Include any signature inside your template.

`--html-body` bypasses the renderer's tag-filter and other safety checks; the operator owns the consequences. Do not pass user-controlled HTML through this flag.

## Sent storage

Sent records under `sent/<mailbox>/` always store the **Markdown source** (or the literal text for `--text-only`, or the `--body` text part for `--html-body`). The recipient's HTML view is recoverable by re-running the same renderer on the stored Markdown — output is deterministic given the pinned `comrak` version.

The frontmatter declares the wire shape so an operator browsing `sent/` can tell at a glance what the recipient saw:

| `outbound_format` | Wire shape | Sent body content |
|-------------------|------------|-------------------|
| `"markdown"` | `multipart/alternative` (text + rendered HTML) | Markdown source verbatim (signature appended before render) |
| `"text"` | single-part `text/plain` | literal text verbatim |
| `"html"` | `multipart/alternative` (text + custom HTML) | the `--body` text part (custom HTML is **not** stored) |

The `outbound_format` field appears immediately after `outbound = true` in the Outbound block of the frontmatter:

```toml
outbound = true
outbound_format = "markdown"
delivery_status = "delivered"
```

Pre-feature sent records lack the field; on read, those default to `"text"` (the historic single-part `text/plain` shape) so legacy records keep parsing cleanly.

**No `.html` sibling file is ever written.** Operators who need a record of the exact custom HTML they sent should keep their template under version control — the `--html-body` payload is not persisted by AIMX.

## Determinism

The renderer is deterministic: the same Markdown input produces byte-identical HTML output across two invocations. This is what justifies dropping the rendered HTML from sent storage — the recipient's view is recoverable. `comrak` is pinned to an exact patch version in `Cargo.toml` (currently `=0.52.0`); bumping it requires re-blessing the renderer fixtures and noting the change in the release notes.

CI defends the determinism guarantee with two checked-in fixtures (`tests/fixtures/markdown/briefing-5kb.md` and `tests/fixtures/markdown/report-50kb.md`) and their pinned expected outputs. A `comrak` bump that changes whitespace or attribute ordering surfaces at CI time, not in production.

## Worked example

Input Markdown:

````markdown
# Daily briefing — 2026-05-07

## Open positions

| Symbol | Shares | Cost basis |
|--------|-------:|-----------:|
| AAPL   | 100    | $150.00    |
| GOOG   | 50     | $2,800.00  |

## Notes

- Earnings season starts next week.
- See the [shareholder letter](https://example.com/letter) for context.

> The market always overreacts in the short term.

```python
def total(positions):
    return sum(s * p for s, p in positions)
```
````

What the recipient sees:

- **Gmail / Outlook / Apple Mail:** rendered `<h1>`, `<h2>`, a styled `<table>`, a clickable `<a>` link, a `<blockquote>` with a left border, and a `<pre><code>` block with monospace background.
- **Text-only client:** the Markdown source verbatim — readable, with hierarchy preserved by `#` and `##`, the table cells visible as pipes-and-dashes.

What gets stored in `sent/alice/2026-05-07-120000-daily-briefing-2026-05-07.md`:

```markdown
+++
id = "2026-05-07-120000-daily-briefing-2026-05-07"
# ... (other frontmatter fields) ...
outbound = true
outbound_format = "markdown"
delivery_status = "delivered"
+++

# Daily briefing — 2026-05-07
# ... (Markdown source verbatim) ...
```

Re-running the daemon's renderer on the stored body reproduces the recipient's HTML view exactly.
