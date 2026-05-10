# AIMX hero loop — ~10s silent terminal loop for the homepage.
# Render with `vhs hero-loop.tape` from this directory.
# Outputs land at website/public/media/hero-loop.{mp4,webm,poster.png}.

Output ../public/media/hero-loop.mp4
Output ../public/media/hero-loop.webm
Output ../public/media/hero-loop-poster.png

Set FontFamily "IBM Plex Mono"
Set FontSize 16
Set Width 1240
Set Height 720
Set Padding 28
Set TypingSpeed 35ms

# aimx brand palette: paper #F4EEE3, ink #151310, copper #B9531C.
Set Theme { "background": "#F4EEE3", "foreground": "#151310", "cursor": "#B9531C", "black": "#151310", "red": "#B9531C", "green": "#5C5549", "yellow": "#8E3E14", "blue": "#2B2620", "magenta": "#8E3E14", "cyan": "#5C5549", "white": "#EBE3D4", "brightBlack": "#5C5549", "brightRed": "#B9531C", "brightGreen": "#2B2620", "brightYellow": "#B9531C", "brightBlue": "#2B2620", "brightMagenta": "#B9531C", "brightCyan": "#5C5549", "brightWhite": "#151310" }

Type "aimx send alpha@friend.com --subject 'the report' --body 'See attached.'"
Sleep 600ms
Enter
Sleep 1500ms
Type "# delivered. message-id queued."
Sleep 800ms
Enter
Type "ls /var/lib/aimx/sent/alpha/ | tail -1"
Sleep 400ms
Enter
Sleep 1200ms
Type "2026-05-06-1430-3f9a-alpha-the-report.md"
Sleep 2500ms
