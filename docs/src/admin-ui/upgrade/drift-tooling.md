# Drift tooling

Once you've started overriding templates, components, or styles, you
have a *fork*. Forks bit-rot — upstream renames a context key, your
template still references the old one and renders empty silently.
Or upstream restructures a partial, your override stops being
invoked.

The `crap-cms templates` family of subcommands is the toolkit for
keeping forks healthy. Four read-only commands:

| Command | What it does |
|---|---|
| `crap-cms templates list` | Lists every file you can extract — the universe of override targets. |
| `crap-cms templates extract` | Copies an upstream default into your config dir, with a source-version header. |
| `crap-cms templates status` | Reports which of your overrides have drifted from upstream. |
| `crap-cms templates diff` | Shows a unified diff between one of your overrides and its upstream counterpart. |
| `crap-cms templates layout` | Reports old-layout files in your config dir and recommends `git mv` commands to migrate. |

All five run against your config dir; none of them mutate any file
they don't have to (the table below is precise about which commands
do and don't touch the filesystem).

## `templates list` — discover override targets

```
$ crap-cms templates list
```

Prints every file in the embedded defaults — the complete list of
paths you can drop into your config dir to override. Filterable by
type:

```
$ crap-cms templates list --type templates
$ crap-cms templates list --type static
```

`--verbose` shows file sizes and a tree view.

**Mutates filesystem:** no.

## `templates extract` — start from a copy of upstream

```
$ crap-cms templates extract layout/base.hbs
```

Copies the embedded default for `layout/base.hbs` into
`<config_dir>/templates/layout/base.hbs`. Prepends a
`{{!-- crap-cms:source 0.1.0-alpha.8 --}}` comment so `templates
status` can detect drift later.

Multiple paths are allowed:

```
$ crap-cms templates extract layout/base.hbs styles/main.css
```

Or extract everything (with optional type filter):

```
$ crap-cms templates extract --all
$ crap-cms templates extract --all --type templates
```

**Mutates filesystem:** writes into your config dir. Skips files
that already exist unless `--force` is passed.

## `templates status` — drift overview

```
$ crap-cms templates status
```

Walks your config dir, parses the source-version header from each
override, and reports drift state per file:

```
Templates customization status (config dir: ./crap, running version: 0.1.0-alpha.8)

  ✓ static/styles/themes/themes-acme.css     —  current
  ⚠ templates/layout/base.hbs                —  behind: extracted from 0.1.0-alpha.5
  ✗ templates/old/header.hbs                 —  orphaned: extracted from upstream but no longer exists there
  ·  static/components/custom.js              —  user-original (no upstream counterpart)
  =  templates/auth/login.hbs                 —  pristine (matches upstream)

Summary: 1 current, 1 behind, 0 ahead, 1 pristine, 0 unknown header, 0 no header, 1 orphaned, 1 user-original

Run `crap-cms templates diff <PATH>` to compare a file against upstream.
```

Drift states (icons in the status output):

| Icon | State | Meaning |
|---|---|---|
| `✓` | Current | Header version matches running crap-cms version. |
| `=` | Pristine | Matches upstream byte-for-byte (you extracted but never edited). |
| `⚠` | Behind | Header version is older than running. Re-extract to see what changed upstream. |
| `↑` | Ahead | Header version is newer (downgrade scenario). |
| `?` | NoHeader | No source-version header (hand-written or stripped). No drift visibility for that file — git is your tool. |
| `?` | UnknownVersion | Source-version header present but unparseable as semver. Repair or re-extract. |
| `✗` | OrphanedUpstream | File claims to extend an upstream that no longer exists (deleted / renamed in a later release). |
| `·` | UserOriginal | No upstream counterpart. A wholly user-authored file (custom widget, bespoke theme, slot contribution, custom admin page). Reported informationally; never a warning. |

**Mutates filesystem:** no.

## `templates diff` — see what changed upstream

```
$ crap-cms templates diff templates/layout/base.hbs
```

Prints a unified diff between your overlay file and the embedded
upstream:

```
--- upstream/templates/layout/base.hbs
+++ /path/to/your/config/templates/layout/base.hbs
@@ ... @@
-      <link href="/static/styles/main.css" rel="stylesheet" />
+      <link href="/static/styles/main.css" rel="stylesheet" />
+      <link href="/static/styles/themes/themes-acme.css" rel="stylesheet" />
```

Useful when `templates status` flagged a file as `behind` and you
want to see what changed upstream so you can re-port your edits.

The diff is computed via the [`similar`](https://crates.io/crates/similar)
crate's Myers algorithm — multi-line insertions group correctly
rather than producing lockstep noise.

**Mutates filesystem:** no.

## `templates layout` — migrate to a new layout

```
$ crap-cms templates layout
```

Walks your config dir, identifies files at *old* layout paths
(pre-1.0 reshuffle), and prints an auto-generated migration recipe:

```
Old layout detected (3 files):
  static/list-toolbar.css → static/styles/parts/lists.css
  static/lists.css        → static/styles/parts/lists.css
  static/styles.css       → static/styles/main.css

Recommended migration (run from /path/to/config):
  mkdir -p static/styles static/styles/parts
  git mv static/styles.css static/styles/main.css
  # MERGE — 2 old files into static/styles/parts/lists.css
  cat static/list-toolbar.css static/lists.css > static/styles/parts/lists.css
  git rm static/list-toolbar.css static/lists.css
  git add static/styles/parts/lists.css

After moving, verify these things the tool can't safely rewrite:
  • `import` paths inside moved JS files (relative paths may break).
  • `{{> "path/to/partial"}}` references in HBS (name lookups are safe).
  • `@import url(...)` references in moved CSS files.
  • `<link>` / `<script>` URLs in any layout HBS files you've overridden.

Then run `crap-cms templates status` to confirm drift visibility re-attaches.
```

Includes `git mv` commands for simple moves and `cat ... > ...` /
`git rm` recipes for files that *merge* into a single new file
(e.g., `lists.css` + `list-toolbar.css` → `parts/lists.css`).

**Mutates filesystem:** no — read-only. The recipe describes; you
transform. The tool is honest about what it can't safely do (rewrite
imports inside moved files), and lists those manual verifications.

See [Migrating from the old layout](migrating-from-old-layout.md)
for the full background on the pre-1.0 reshuffle and the
compatibility-alias behavior.

## Workflow — typical fork-maintenance cycle

```
# After upgrading crap-cms to a new release:
$ cargo install crap-cms --version 0.1.0-alpha.9

# 1. See what's drifted in your config dir:
$ crap-cms templates status

# 2. For each file flagged as "behind", inspect the upstream changes:
$ crap-cms templates diff templates/layout/base.hbs

# 3. Re-port your edits onto a fresh extract, OR keep your overlay
#    if upstream's changes don't affect your customizations:
$ crap-cms templates extract --force layout/base.hbs
# Re-apply your edits to the freshly-extracted copy.

# 4. Verify no orphaned overrides:
$ crap-cms templates status
# Should show all overrides as ✓ current or = pristine.
```

The workflow scales: a config dir with 50 overrides, run `status`
once, focus on the 3 files flagged `behind` or `orphaned`. The other
47 are still tracking upstream cleanly.

## Source-version header

When `templates extract` writes a file, it prepends a one-line
header that records the upstream version:

```hbs
{{!-- crap-cms:source 0.1.0-alpha.8 --}}
<!DOCTYPE html>
...
```

```css
/* crap-cms:source 0.1.0-alpha.8 */
:root {
  ...
}
```

```js
/* crap-cms:source 0.1.0-alpha.8 */
import { ... } from '...';
...
```

The header is **purely metadata** — the file works identically with
or without it. `templates status` reads the header to compute drift.
A user who hand-writes an override (or strips the header for
aesthetic reasons) loses drift visibility for that file but keeps
the override functional.

If you regret stripping a header, just re-extract with `--force`
into a temporary directory, copy the header line back, and discard
the rest.
