# Static Files

Static files (CSS, JS, fonts, images) use the same overlay pattern as templates.

## Overlay Pattern

1. **Config directory** — `<config_dir>/static/` is served via `tower_http::ServeDir`
2. **Compiled defaults** — built into the binary via `include_dir!` macro

If a file exists in both locations, the config directory version wins.

## Accessing Static Files

All static files are served under `/static/`:

```html
<link rel="stylesheet" href="/static/styles.css">
<script type="module" src="/static/components/index.js"></script>
```

## Overriding Static Files

Place files in your config directory's `static/` folder:

```
my-project/
└── static/
    └── css/
        └── custom.css    # served at /static/css/custom.css
```

To override a built-in file (e.g., the main stylesheet or a JS component), use the same path:

```
my-project/
└── static/
    ├── styles.css                  # overrides the compiled-in stylesheet
    └── components/
        └── toast.js                # overrides just the toast component
```

## Compiled-In Files

Default static files are compiled into the binary using the `include_dir!` macro. This means:

- The binary is self-contained — no external files needed for the default admin UI
- **After modifying compiled-in static files, you must run `cargo build`** for changes to take effect
- Config directory overrides don't require rebuilding

## MIME Types

Content-Type headers are automatically detected from file extensions using the `mime_guess` crate.

## Cache model

Every static response carries:

- **`Cache-Control: public, no-cache`** — the browser **may** cache the
  response, but **must revalidate** with the server on every subsequent
  request before reusing the cached copy. (Despite the name, `no-cache`
  is not "don't cache" — that would be `no-store`.)
- **`ETag`** — embedded files use `BUILD_HASH` (a content-derived hash
  baked at compile time, recomputed on every `cargo build`); config-dir
  files use the `ServeDir`-default mtime/size token.

Each subsequent request sends `If-None-Match: <previous-etag>`; the
server replies `304 Not Modified` (no body) when the ETag still matches,
or `200` with the new content when it doesn't. Browsers reuse the
cached body in the 304 case, so the network cost is just the
conditional-GET round-trip — no asset re-download.

This model intentionally **prioritises overlay-author DX over a few
hundred bytes of revalidation traffic**:

- **Production deploy** — `cargo build` changes `BUILD_HASH`, every
  embedded file's ETag changes, the next page load gets fresh content.
  Config-dir files keep their unchanged mtime ETags and stay 304s.
- **Config-dir edit** — overlay author saves a file in
  `<config_dir>/static/`, mtime changes, `ServeDir` ETag changes, the
  next request returns the new content. **No restart, no rebuild
  required.**

### Why no `?v=BUILD_HASH` query string

You may have seen `<script src="…?v={{crap.build_hash}}">` in older
admin templates. We dropped it: with the ETag-based revalidation above,
it didn't actually change anything. ES-module imports inside the
JavaScript files (`import './toast.js'`) and CSS `@import url("…")`
statements don't carry the query string anyway, so they always
revalidate via ETag — and that's exactly what we want, because adding
`?v=…` to a long-cached versioned URL would defeat the overlay model
(the new URL would never invalidate when an overlay author edits
`<config_dir>/static/foo.js`).

If your CDN or reverse proxy rewrites `Cache-Control: no-cache` to
something more aggressive (some old corporate proxies do), you'll need
to either configure the proxy to honour the directive or strip it from
intermediate caches with a `Vary` / `Surrogate-Control` rule appropriate
to your stack.

## Technical Note

The embedded fallback handler uses `axum::http::Uri` extraction (not `axum::extract::Path`) because it runs as a `ServeDir` fallback service where no route parameters are defined.
