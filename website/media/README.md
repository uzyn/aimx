# Demo media sources

This directory holds the source files for the demo videos embedded on
the homepage. The rendered outputs live under `../public/media/` and
are committed binaries — CI never re-renders them.

## Files

- `hero-loop.tape` — ~10s silent loop shown under the hero headline.
- `tour-60s.tape` — 60-second captioned walk-through of setup → send → land on disk → inbound → reply.
- `../public/media/tour-60s.vtt` — caption track for the 60-second tour.

## Render

Requires [`vhs`](https://github.com/charmbracelet/vhs) and `ffmpeg`.

```sh
# From the website/ directory:
make media

# Or render individually:
cd media && vhs hero-loop.tape
cd media && vhs tour-60s.tape
```

Outputs land at `../public/media/{hero-loop,tour-60s}.{mp4,webm,poster.png}`.
The poster file is the last frame of the rendered video — vhs writes it
automatically alongside the MP4 and WebM.

## Iterating

The 60-second tour storyboard is a **storyboard, not a transcript**. Each
frame is a representative CLI moment. The actual `aimx` outputs in production
will differ slightly (real DKIM keys, real message IDs, real timestamps).
Keep the captions and the on-screen text aligned when iterating: if you
shorten Frame 4 by 1 second, shorten the matching `tour-60s.vtt` cue.

## Brand match

The terminal theme in both tapes uses the AIMX palette:

- `background`: `#F4EEE3` (paper)
- `foreground`: `#151310` (ink)
- `cursor`: `#B9531C` (copper)

Font: IBM Plex Mono. Don't change these without updating
`docs/branding.md` and the homepage CSS to match.
