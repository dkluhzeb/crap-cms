# Themes

The admin UI includes built-in theme support with multiple color schemes.

## Built-In Themes

The admin UI ships with four themes:

| Theme | Description |
|-------|-------------|
| Light | Default light theme |
| Dark | Dark mode |
| Navy | Dark blue theme |
| Solarized | Solarized color scheme |

## Switching Themes

Themes can be switched via the admin UI's theme picker. The selected theme is persisted in `localStorage`.

## How Themes Work

Themes use CSS custom properties defined in `:root`. Each theme overrides the base variables:

```css
:root {
    --color-bg: #ffffff;
    --color-text: #1a1a1a;
    --color-surface: #f5f5f5;
    --color-border: #e0e0e0;
    /* ... */
}

[data-theme="dark"] {
    --color-bg: #1a1a1a;
    --color-text: #f5f5f5;
    --color-surface: #2d2d2d;
    --color-border: #404040;
    /* ... */
}
```

## Custom Themes

To create a custom theme:

1. Create a CSS file in your config directory's `static/css/`:

```css
/* static/css/my-theme.css */
[data-theme="custom"] {
    --color-bg: #fdf6e3;
    --color-text: #657b83;
    /* override all relevant variables */
}
```

2. Override the base `styles.css` to import your theme, or add a `<link>` in a template override.

## CSS Custom Properties

Key variables available for theming:

| Variable | Description |
|----------|-------------|
| `--color-bg` | Page background |
| `--color-text` | Primary text color |
| `--color-surface` | Card/panel backgrounds |
| `--color-border` | Border colors |
| `--color-primary` | Primary accent color |
| `--color-primary-hover` | Primary hover state |
| `--color-sidebar-bg` | Sidebar background |
| `--shadow-sm`, `--shadow-md`, `--shadow-lg` | Box shadows |
| `--radius-sm`, `--radius-md`, `--radius-lg` | Border radii |
| `--font-size-*` | Font sizes (xs, sm, base, md, lg, xl, 2xl) |
