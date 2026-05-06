# AIMX 60-second tour — silent, captioned via WebVTT (write tour-60s.vtt
# alongside the rendered MP4 with the timing below).
#
# Render with `vhs tour-60s.tape` from this directory.
# Outputs land at website/public/media/tour-60s.{mp4,webm,poster.png}.
# Captions: hand-author website/public/media/tour-60s.vtt to match.
#
# Storyboard (timing assumes default TypingSpeed; tweak Sleep values
# while watching playback to land each beat at the right second):
#
#   00:00 → 00:07  DNS one-liner: dig MX returns the VPS hostname
#   00:07 → 00:14  Setup: aimx setup prints DKIM keypair + systemd unit
#   00:14 → 00:22  Send via aimx CLI (proxy for MCP send_email)
#   00:22 → 00:30  Land on disk: tree + cat the resulting Markdown
#   00:30 → 00:38  Inbound arrival + on_receive hook fires
#   00:38 → 00:46  Read the inbound message (proxy for MCP read_email)
#   00:46 → 00:54  Reply (proxy for MCP reply_email) lands in sent/
#   00:54 → 00:60  Close card

Output ../public/media/tour-60s.mp4
Output ../public/media/tour-60s.webm
Output ../public/media/tour-60s-poster.png

Set FontFamily "IBM Plex Mono"
Set FontSize 16
Set Width 1240
Set Height 720
Set Padding 28
Set TypingSpeed 30ms

Set Theme { "background": "#F4EEE3", "foreground": "#151310", "cursor": "#B9531C", "black": "#151310", "red": "#B9531C", "green": "#5C5549", "yellow": "#8E3E14", "blue": "#2B2620", "magenta": "#8E3E14", "cyan": "#5C5549", "white": "#EBE3D4", "brightBlack": "#5C5549", "brightRed": "#B9531C", "brightGreen": "#2B2620", "brightYellow": "#B9531C", "brightBlue": "#2B2620", "brightMagenta": "#B9531C", "brightCyan": "#5C5549", "brightWhite": "#151310" }

# 0–7s — DNS one-liner
Type "dig agent.example.com MX +short"
Sleep 400ms
Enter
Sleep 1500ms
Type "10 agent.example.com."
Sleep 3000ms
Ctrl+L

# 7–14s — Setup
Type "sudo aimx setup agent.example.com"
Sleep 400ms
Enter
Sleep 600ms
Type "→ generating DKIM keypair (RSA 2048)"
Enter
Sleep 200ms
Type "→ writing /etc/aimx/config.toml"
Enter
Sleep 200ms
Type "→ installing systemd unit aimx.service"
Enter
Sleep 200ms
Type "→ DNS records to publish:"
Enter
Type "  agent.example.com.    MX  10  agent.example.com."
Enter
Type "  default._domainkey.agent.example.com.  TXT  \"v=DKIM1; k=rsa; p=...\""
Enter
Sleep 2500ms
Ctrl+L

# 14–22s — Send via aimx CLI
Type "aimx send alpha@friend.com --subject 'the report' --body 'See attached.'"
Sleep 400ms
Enter
Sleep 800ms
Type "→ DKIM signed"
Enter
Type "→ MX lookup for friend.com → mx1.friend.com (10)"
Enter
Type "→ delivered.  message-id <a8f...@agent.example.com>"
Enter
Sleep 2500ms
Ctrl+L

# 22–30s — Land on disk
Type "tree /var/lib/aimx/sent/alpha/ -L 1 | tail -3"
Sleep 400ms
Enter
Sleep 700ms
Type "  └── 2026-05-06-1430-3f9a-the-report.md"
Enter
Sleep 800ms
Type "cat /var/lib/aimx/sent/alpha/2026-05-06-1430-3f9a-the-report.md"
Sleep 400ms
Enter
Sleep 700ms
Type "+++"
Enter
Type "from = \"alpha@agent.example.com\""
Enter
Type "to   = [\"alpha@friend.com\"]"
Enter
Type "subject = \"the report\""
Enter
Type "+++"
Enter
Type "See attached."
Enter
Sleep 2500ms
Ctrl+L

# 30–38s — Inbound arrival + on_receive hook fires
Type "journalctl -u aimx -f --since '5 seconds ago'"
Sleep 400ms
Enter
Sleep 700ms
Type "Jan 06 14:32:11 host aimx[1234]: SMTP RCPT alpha@agent.example.com"
Enter
Sleep 200ms
Type "Jan 06 14:32:11 host aimx[1234]: ingest -> /var/lib/aimx/inbox/alpha/2026-05-06-1432-7c2e.md"
Enter
Sleep 200ms
Type "Jan 06 14:32:11 host aimx[1234]: hook on_receive: route_reply  exit_code=0  duration=42ms"
Enter
Sleep 2500ms
Ctrl+L

# 38–46s — Read the inbound message
Type "cat /var/lib/aimx/inbox/alpha/2026-05-06-1432-7c2e.md"
Sleep 400ms
Enter
Sleep 700ms
Type "+++"
Enter
Type "from    = \"human@friend.com\""
Enter
Type "to      = [\"alpha@agent.example.com\"]"
Enter
Type "subject = \"Re: the report\""
Enter
Type "trusted = \"true\""
Enter
Type "+++"
Enter
Type "Looks good. Send the audit version too."
Enter
Sleep 2500ms
Ctrl+L

# 46–54s — Reply lands in sent/
Type "aimx send human@friend.com --in-reply-to '<7c2e...@friend.com>' --body 'Audit version attached.'"
Sleep 400ms
Enter
Sleep 800ms
Type "→ DKIM signed"
Enter
Type "→ delivered.  message-id <b1c...@agent.example.com>"
Enter
Type "→ /var/lib/aimx/sent/alpha/2026-05-06-1433-b1c0.md"
Enter
Sleep 2500ms
Ctrl+L

# 54–60s — Close card
Type "# AIMX — SMTP for AI agents.  No middleman."
Enter
Type "# curl -fsSL aimx.email/install.sh | sh"
Enter
Type "# aimx.email/book/"
Sleep 4000ms
