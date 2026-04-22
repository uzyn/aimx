# Release Notes

Version-by-version changelog of operator-visible behavior changes. Use this as the canonical source for "what changed" between aimx releases; individual book chapters describe the current behavior only.

## v1 (pre-launch)

### Behavioral shifts

- **`aimx mailboxes create <name>` without `--owner` now prompts interactively** (agent-integration Sprint 3). Previously this command hard-errored with a `useradd` hint when the local-part of the address did not resolve to an existing Linux user. The new behavior:
  - On a TTY with `AIMX_NONINTERACTIVE` unset: the command prompts for the Linux user that should own the mailbox, re-prompts up to five times if the entered username does not resolve via `getpwnam`, and finally errors with an actionable `useradd` hint if every attempt fails.
  - With `AIMX_NONINTERACTIVE=1`: the command errors immediately (exit 1) with the same hint whenever the local-part default does not resolve. Scripted installers should set this variable so the command never blocks.
  - With a piped / closed stdin AND `AIMX_NONINTERACTIVE` unset: the prompt loop burns its five attempts (each `read_line` returns EOF immediately) before erroring. Still fails fast, just noisier than the non-interactive path. Set `AIMX_NONINTERACTIVE=1` whenever you pipe input to avoid the extra output.

- **`aimx setup` drops the `--non-interactive` flag** (agent-integration Sprint 7.5). The legacy hook-template checkbox phase was removed in Sprint 3 so the flag no longer gated any interactive prompt. Scripts that passed `--non-interactive` must drop the argument; the `AIMX_NONINTERACTIVE=1` environment variable remains the canonical way to force non-interactive behavior for helpers that still support it (today: `aimx mailboxes create`).
