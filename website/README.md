# aimx.email — website sources

Static site for `aimx.email`. The homepage lives in `public/`. The user guide is an mdBook build of the sibling `book/` directory.

## Build

```
cd website
make build
```

Produces `website/dist/`:

```
dist/
├── index.html              # homepage
├── favicon.svg
├── robots.txt
├── assets/aimx-pigeon.svg
└── book/                   # mdBook render of ../book
    ├── index.html
    └── ...
```

## Local preview

```
make serve
```

Opens a plain Python static server on `http://localhost:8000/`. The guide is at `http://localhost:8000/book/`.

## Deploy

`dist/` is a self-contained static tree. Drop it behind any HTTP server — Caddy, nginx, Cloudflare Pages, GitHub Pages, S3 + CloudFront. No runtime dependencies.

## Layout

```
website/
├── Makefile              # build / serve / clean
├── README.md             # this file
├── book.toml             # mdBook config; src = ../book
├── public/               # raw homepage assets copied verbatim into dist/
│   ├── index.html
│   ├── favicon.svg
│   └── robots.txt
├── styles/               # additional CSS layered on top of mdBook defaults
│   ├── palette.css       # brand colours (light + dark), remapped onto
│   │                     # mdBook's built-in theme classes
│   ├── typography.css    # Fraunces / IBM Plex rules per branding §2.2 + §5.3
│   └── chrome.css        # sidebar, topbar, search, menu chrome
└── theme/                # mdBook theme overrides
    ├── head.hbs          # injects Google Fonts + theme-color meta
    ├── favicon.svg
    └── favicon.png
```

## Requirements

- `mdbook` (tested with 0.5.2)
- `make`
- `python3` (only for `make serve`)

## Branding source of truth

All colour, type, and voice choices are governed by `docs/branding.md` in the parent project. When updating this site, reconcile against that document first.
