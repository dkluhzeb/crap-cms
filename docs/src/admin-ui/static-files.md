# Static Files

Static files (CSS, JS, fonts, images) use the same overlay pattern as templates.

## Overlay Pattern

1. **Config directory** — `<config_dir>/static/` is served via `tower_http::ServeDir`
2. **Compiled defaults** — built into the binary via `include_dir!` macro

If a file exists in both locations, the config directory version wins.

## Accessing Static Files

All static files are served under `/static/`:

```html
<link rel="stylesheet" href="/static/css/styles.css">
<script src="/static/js/components.js" defer></script>
```

## Overriding Static Files

Place files in your config directory's `static/` folder:

```
my-project/
└── static/
    └── css/
        └── custom.css    # served at /static/css/custom.css
```

To override a built-in file (e.g., the main stylesheet), use the same path:

```
my-project/
└── static/
    └── css/
        └── styles.css    # overrides the compiled-in styles.css
```

## Compiled-In Files

Default static files are compiled into the binary using the `include_dir!` macro. This means:

- The binary is self-contained — no external files needed for the default admin UI
- **After modifying compiled-in static files, you must run `cargo build`** for changes to take effect
- Config directory overrides don't require rebuilding

## MIME Types

Content-Type headers are automatically detected from file extensions using the `mime_guess` crate.

## Technical Note

The embedded fallback handler uses `axum::http::Uri` extraction (not `axum::extract::Path`) because it runs as a `ServeDir` fallback service where no route parameters are defined.
