# Themes

The admin UI includes built-in theme support with multiple color schemes.

## Built-In Themes

| Theme | Type | Description |
|-------|------|-------------|
| Light | light | Default theme. Clean blue-accented light design. |
| Rosé Pine Dawn | light | Warm, muted light theme based on the [Rosé Pine](https://rosepinetheme.com/) palette. |
| Tokyo Night | dark | Cool blue-purple dark theme. |
| Catppuccin Mocha | dark | Pastel-accented dark theme from the [Catppuccin](https://catppuccin.com/) palette. |
| Gruvbox Dark | dark | Warm retro dark theme with orange accents. |

## Switching Themes

Click the palette icon in the admin header to open the theme picker. The selected theme is persisted in `localStorage` and restored on page load (no flash of unstyled content).

## How Themes Work

The default Light theme is defined in `:root` CSS custom properties in `styles.css`. Additional themes live in `themes.css` and override these variables using `html[data-theme="<name>"]` selectors.

> **Theme values are static CSS.** Every theme's custom-property values are hardcoded in CSS files — either the built-in `themes.css` or an operator-supplied stylesheet under `<config_dir>/static/`. Crap CMS does **not** read theme values from the database, `crap.toml`, or any other operator input and inject them into the stylesheet. There is no template/string interpolation in the theme pipeline, so there is no CSS-injection surface from operator configuration.

```css
/* Default light theme (styles.css) */
:root {
  color-scheme: light;
  --color-primary: #1677ff;
  --bg-body: #f4f7fc;
  --text-primary: rgba(0, 0, 0, 0.88);
  /* ... */
}

/* Dark theme override (themes.css) */
html[data-theme="tokyo-night"] {
  color-scheme: dark;
  --color-primary: #7aa2f7;
  --bg-body: #1a1b26;
  --text-primary: #c0caf5;
  /* ... */
}
```

## Custom Themes

To create a custom theme, add a CSS file in your config directory's `static/` folder and override the theme picker template.

### 1. Create the theme CSS

Create `static/themes-custom.css` in your config directory:

```css
html[data-theme="my-theme"] {
  color-scheme: light; /* or dark */

  /* Primary accent */
  --color-primary: #6366f1;
  --color-primary-hover: #818cf8;
  --color-primary-active: #4f46e5;
  --color-primary-bg: rgba(99, 102, 241, 0.08);

  /* Danger/error */
  --color-danger: #ef4444;
  --color-danger-hover: #f87171;
  --color-danger-active: #dc2626;
  --color-danger-bg: rgba(239, 68, 68, 0.08);

  /* Success */
  --color-success: #22c55e;
  --color-success-bg: rgba(34, 197, 94, 0.08);

  /* Warning */
  --color-warning: #f59e0b;
  --color-warning-bg: rgba(245, 158, 11, 0.08);

  /* Text */
  --text-primary: #1e293b;
  --text-secondary: #475569;
  --text-tertiary: #94a3b8;
  --text-on-primary: #ffffff;

  /* Surfaces */
  --bg-body: #f8fafc;
  --bg-surface: #ffffff;
  --bg-elevated: #f1f5f9;
  --bg-hover: rgba(0, 0, 0, 0.04);

  /* Borders */
  --border-color: rgba(0, 0, 0, 0.08);
  --border-color-hover: rgba(0, 0, 0, 0.15);

  /* Shadows */
  --shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.05);
  --shadow-md: 0 4px 12px rgba(0, 0, 0, 0.08);
  --shadow-lg: 0 8px 24px rgba(0, 0, 0, 0.12);

  /* Inputs */
  --input-bg: #ffffff;
  --input-border: rgba(0, 0, 0, 0.12);

  /* Header */
  --header-bg: #f1f5f9;
  --header-border: rgba(0, 0, 0, 0.08);

  /* Sidebar */
  --sidebar-bg: transparent;
  --sidebar-active-bg: rgba(99, 102, 241, 0.1);
  --sidebar-active-text: #6366f1;
}
```

### 2. Import in a styles override

Create `static/styles.css` in your config directory (the static overlay will serve it instead of the built-in version). Copy the original and add your import at the bottom:

```css
@import url("themes-custom.css");
```

### 3. Add the picker option

Override `templates/layout/header.hbs` and add a button to the `.theme-picker__dropdown`:

```html
<button type="button" class="theme-picker__option" data-theme-value="my-theme">
  <span class="theme-picker__swatch" style="background:#f8fafc"></span>
  My Theme
</button>
```

## CSS Custom Properties Reference

All variables that themes should override:

| Variable | Description |
|----------|-------------|
| **Colors** | |
| `--color-primary` | Primary accent color |
| `--color-primary-hover` | Primary hover state |
| `--color-primary-active` | Primary active/pressed state |
| `--color-primary-bg` | Primary tinted background (low opacity) |
| `--color-danger` | Error/destructive color |
| `--color-danger-hover` | Danger hover state |
| `--color-danger-active` | Danger active state |
| `--color-danger-bg` | Danger tinted background |
| `--color-success` | Success color |
| `--color-success-bg` | Success tinted background |
| `--color-warning` | Warning color |
| `--color-warning-bg` | Warning tinted background |
| **Text** | |
| `--text-primary` | Primary text |
| `--text-secondary` | Secondary/muted text |
| `--text-tertiary` | Disabled/hint text |
| `--text-on-primary` | Text on primary-colored backgrounds |
| **Surfaces** | |
| `--bg-body` | Page background |
| `--bg-surface` | Card/panel background |
| `--bg-elevated` | Elevated surface (modals, popovers) |
| `--bg-hover` | Generic hover background |
| **Borders** | |
| `--border-color` | Default border color |
| `--border-color-hover` | Border hover color |
| **Shadows** | |
| `--shadow-sm` | Small shadow (cards) |
| `--shadow-md` | Medium shadow (dropdowns) |
| `--shadow-lg` | Large shadow (modals) |
| **Inputs** | |
| `--input-bg` | Form input background |
| `--input-border` | Form input border |
| `--select-arrow` | Select dropdown arrow (SVG data URL) |
| **Layout** | |
| `--header-bg` | Header background |
| `--header-border` | Header bottom border |
| `--sidebar-bg` | Sidebar background |
| `--sidebar-active-bg` | Active sidebar item background |
| `--sidebar-active-text` | Active sidebar item text color |

Variables **not** typically overridden by themes (inherited from `:root`): `--radius-*`, `--space-*`, `--transition-*`, `--text-xs` through `--text-2xl`, `--sidebar-width`, `--input-height`.
