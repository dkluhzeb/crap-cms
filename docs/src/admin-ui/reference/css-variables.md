<!-- GENERATED FILE — do not edit. Regenerate with `cargo xtask gen-doc-tables`. -->

# CSS Variables

The admin UI uses CSS custom properties for every design decision —
spacing, color, typography, sizes, shadows, transitions, and
component-specific knobs. Themes override these on
`html[data-theme="…"]`, so any component reading the variables
automatically participates in theming.

This reference is generated from
[`static/styles/tokens.css`](https://github.com/dkluhs/crap-cms/blob/main/static/styles/tokens.css)
— the tokens file is the contract. Every token below is stable
theming surface; sizes derive from `--base` with small multipliers,
so changing `--base` rescales the whole admin proportionally.

## Colors - Primary

| Token | Value | Notes |
|---|---|---|
| `--color-primary` | `#1677ff` |  |
| `--color-primary-hover` | `#4096ff` |  |
| `--color-primary-active` | `#0958d9` |  |
| `--color-primary-bg` | `rgba(22, 119, 255, 0.06)` |  |

## Colors - Danger

| Token | Value | Notes |
|---|---|---|
| `--color-danger` | `#ff4d4f` |  |
| `--color-danger-hover` | `#ff7875` |  |
| `--color-danger-active` | `#d9363e` |  |
| `--color-danger-bg` | `rgba(255, 77, 79, 0.06)` |  |

## Colors - Success

| Token | Value | Notes |
|---|---|---|
| `--color-success` | `#52c41a` |  |
| `--color-success-bg` | `rgba(82, 196, 26, 0.06)` |  |

## Colors - Warning

| Token | Value | Notes |
|---|---|---|
| `--color-warning` | `#faad14` |  |
| `--color-warning-bg` | `rgba(250, 173, 20, 0.06)` |  |

## Text

| Token | Value | Notes |
|---|---|---|
| `--text-primary` | `rgba(0, 0, 0, 0.88)` |  |
| `--text-secondary` | `rgba(0, 0, 0, 0.65)` |  |
| `--text-tertiary` | `rgba(0, 0, 0, 0.45)` |  |
| `--text-on-primary` | `#fff` |  |

## Surfaces

| Token | Value | Notes |
|---|---|---|
| `--bg-body` | `#f4f7fc` |  |
| `--bg-surface` | `#f8f9fb` |  |
| `--bg-elevated` | `#fff` |  |
| `--bg-hover` | `rgba(0, 0, 0, 0.04)` |  |

## Borders

| Token | Value | Notes |
|---|---|---|
| `--border-color` | `rgba(0, 0, 0, 0.08)` |  |
| `--border-color-hover` | `rgba(0, 0, 0, 0.15)` |  |

## Shadows

| Token | Value | Notes |
|---|---|---|
| `--shadow-sm` | `0 1px 2px rgba(0, 0, 0, 0.04)` |  |
| `--shadow-md` | `0 2px 8px rgba(0, 0, 0, 0.06)` |  |
| `--shadow-lg` | `0 4px 16px rgba(0, 0, 0, 0.08)` |  |

## Radii

| Token | Value | Notes |
|---|---|---|
| `--radius-sm` | `4px` |  |
| `--radius-md` | `6px` |  |
| `--radius-lg` | `8px` |  |
| `--radius-xl` | `12px` |  |
| `--radius-full` | `9999px` |  |

## Base unit

| Token | Value | Notes |
|---|---|---|
| `--base` | `0.25rem` |  |

## Spacing (base × n)

| Token | Value | Notes |
|---|---|---|
| `--space-2xs` | `calc(var(--base) * 0.5)` |  |
| `--space-xs` | `var(--base)` |  |
| `--space-xs2` | `calc(var(--base) * 1.5)` |  |
| `--space-sm` | `calc(var(--base) * 2)` |  |
| `--space-sm2` | `calc(var(--base) * 2.5)` |  |
| `--space-md` | `calc(var(--base) * 3)` |  |
| `--space-lg` | `calc(var(--base) * 4)` |  |
| `--space-xl` | `calc(var(--base) * 6)` |  |
| `--space-2xl` | `calc(var(--base) * 8)` |  |

## Icon sizes (base × n)

| Token | Value | Notes |
|---|---|---|
| `--icon-xs` | `calc(var(--base) * 3.5)` |  |
| `--icon-sm` | `calc(var(--base) * 4)` |  |
| `--icon-md` | `calc(var(--base) * 4.5)` |  |
| `--icon-lg` | `calc(var(--base) * 6)` |  |
| `--icon-xl` | `calc(var(--base) * 12)` |  |

## Control sizes (base × n)

| Token | Value | Notes |
|---|---|---|
| `--control-sm` | `calc(var(--base) * 7)` |  |
| `--control-md` | `calc(var(--base) * 8)` |  |
| `--control-lg` | `calc(var(--base) * 9)` |  |

## Transitions

| Token | Value | Notes |
|---|---|---|
| `--transition-fast` | `0.15s ease` |  |
| `--transition-normal` | `0.25s ease` |  |
| `--transition-smooth` | `0.3s cubic-bezier(0.215, 0.61, 0.355, 1)` |  |

## Font sizes

| Token | Value | Notes |
|---|---|---|
| `--text-xs` | `0.75rem` |  |
| `--text-sm` | `0.8125rem` |  |
| `--text-base` | `0.875rem` |  |
| `--text-lg` | `1rem` |  |
| `--text-xl` | `1.125rem` |  |
| `--text-2xl` | `1.375rem` |  |

## Layout

| Token | Value | Notes |
|---|---|---|
| `--sidebar-width` | `calc(var(--base) * 52)` |  |
| `--header-height` | `calc(var(--base) * 10)` |  |

## Semantic - Sidebar

| Token | Value | Notes |
|---|---|---|
| `--sidebar-bg` | `transparent` |  |
| `--sidebar-active-bg` | `var(--color-primary-bg)` |  |
| `--sidebar-active-text` | `var(--color-primary)` |  |

## Semantic - Surfaces

| Token | Value | Notes |
|---|---|---|
| `--surface-primary` | `var(--bg-elevated)` |  |
| `--surface-secondary` | `var(--bg-surface)` |  |
| `--surface-hover` | `var(--bg-hover)` |  |

## Semantic - Borders

| Token | Value | Notes |
|---|---|---|
| `--border-primary` | `var(--border-color-hover)` |  |
| `--border-default` | `var(--border-color)` |  |

## Semantic - Accent

| Token | Value | Notes |
|---|---|---|
| `--accent-primary` | `var(--color-primary)` |  |
| `--accent-primary-bg` | `var(--color-primary-bg)` |  |

## Semantic - Inputs

| Token | Value | Notes |
|---|---|---|
| `--input-bg` | `var(--bg-elevated)` |  |
| `--input-border` | `var(--border-color-hover)` |  |
| `--input-height` | `var(--control-lg)` |  |

## Semantic - Buttons

| Token | Value | Notes |
|---|---|---|
| `--button-height` | `var(--control-lg)` |  |
| `--button-height-sm` | `var(--control-sm)` |  |

## Common layout sizes

| Token | Value | Notes |
|---|---|---|
| `--dropdown-max-height` | `calc(var(--base) * 60)` |  |
| `--preview-max-width` | `calc(var(--base) * 50)` |  |
| `--preview-max-width-lg` | `calc(var(--base) * 75)` |  |
| `--padding-with-icon` | `calc(var(--base) * 10)` |  |
| `--max-width-form` | `calc(var(--base) * 100)` |  |
| `--select-arrow` | `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='rgba(0,0,0,0.45)' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M6 9l6 6 6-6'/%3E%3C/svg%3E")` |  |

## Semantic - Header

| Token | Value | Notes |
|---|---|---|
| `--header-bg` | `var(--bg-elevated)` |  |
| `--header-border` | `var(--border-color)` |  |

## Code-editor syntax-highlight palette (light defaults; dark themes

| Token | Value | Notes |
|---|---|---|
| `--code-keyword` | `#708` |  |
| `--code-string` | `#a11` |  |
| `--code-number` | `#164` |  |
| `--code-comment` | `#888` |  |
| `--code-atom` | `#219` |  |
| `--code-property` | `#00f` |  |
| `--code-function` | `#00c` |  |
| `--code-definition` | `#00f` |  |
| `--code-type` | `#085` |  |
| `--code-operator` | `#708` |  |
| `--code-regexp` | `#a11` |  |
| `--code-meta` | `#888` |  |
| `--code-tag` | `#708` |  |
| `--code-attribute` | `#00c` |  |
| `--code-heading` | `#708` |  |
| `--code-link` | `#00c` |  |

Tokens are declared under `:root` with `color-scheme: light`;
dark values live in `static/styles/themes/default.css` under
`html[data-theme="dark"]`. Adding a token is a public surface
change — document intent here by keeping the group comment in
`tokens.css` accurate, and run `crap-cms theme validate` when a
contrast pair changes.
