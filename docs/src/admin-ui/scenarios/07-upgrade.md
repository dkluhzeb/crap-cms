# Scenario 7: Upgrade crap-cms — what breaks?

**Goal**: upgrade crap-cms to a new release; understand what your
overrides need re-porting and what changed in the embedded
defaults.

**Difficulty**: low, with the right tooling. ~5 minutes for a clean
upgrade with a handful of overrides.

**You'll touch**: nothing structural — this is a tooling-driven
inspection workflow.

## The upgrade workflow

```
# 1. Bump the binary (release-pinned: cargo, distro pkg, container, etc.).
$ cargo install crap-cms --version 0.1.0-alpha.9

# 2. Inspect drift — what overlays of yours are now behind upstream?
$ crap-cms templates status

# 3. For each flagged file, see exactly what upstream changed:
$ crap-cms templates diff templates/layout/base.hbs

# 4. Re-port: extract a fresh copy, then merge your edits in.
$ crap-cms templates extract --force layout/base.hbs

# 5. Verify nothing else regressed:
$ crap-cms templates status
```

That's the loop. Most upgrades won't flag anything — your override
extracted from version 0.1.0-alpha.5 and the upstream is still
0.1.0-alpha.5 for that particular file means no drift.

## What `templates status` tells you

| State | Icon | Meaning | Action |
|---|---|---|---|
| Current | `✓` | Header version matches running. | None. |
| Pristine | `=` | Matches upstream byte-for-byte. | None — you extracted but never edited. |
| Behind | `⚠` | Header version older than running. | Run `templates diff` to see upstream changes. Decide whether to re-port. |
| Ahead | `↑` | Header version newer than running. | Downgrade scenario — your override targets a future version. Update your environment or revert. |
| NoHeader | `?` | No source-version header. | Hand-written or stripped. No drift visibility — git is your only tool. |
| UnknownVersion | `?` | Source-version header present but unparseable as semver. | Repair the header (or re-extract). |
| OrphanedUpstream | `✗` | File claims to extend an upstream that's gone. | Upstream renamed or removed the file. Your override is now dead — delete or migrate. |
| UserOriginal | `·` | No upstream counterpart. | Custom widget, bespoke theme, etc. Always reported informationally. |

See [drift tooling reference](../upgrade/drift-tooling.md) for
deeper detail.

## Common upgrade patterns

### Pattern A — drift on a template you edited

```
$ crap-cms templates status
  ⚠ templates/layout/base.hbs   —  behind: extracted from 0.1.0-alpha.5

$ crap-cms templates diff templates/layout/base.hbs
--- upstream/templates/layout/base.hbs
+++ /path/to/config/templates/layout/base.hbs
@@ ... @@
-    <link href="/static/styles.css" rel="stylesheet" />
+    <link href="/static/styles/main.css" rel="stylesheet" />
```

Upstream renamed `styles.css` → `styles/main.css`. The
compatibility-alias layer keeps your overlay working today, but
you'll see deprecation warnings in the logs. To clear them:

```
$ crap-cms templates extract --force layout/base.hbs
```

Then re-apply your customizations (sed, manual diff, or a
side-by-side editor) onto the freshly extracted copy. Keep the
new source-version header so future drift detection works.

### Pattern B — orphaned override

```
$ crap-cms templates status
  ✗ templates/old/my-widget.hbs   —  orphaned: extracted from upstream but no longer exists there
```

Upstream deleted or renamed the template. Your override is
referencing nothing. Two options:

- **Delete the override** — if you don't need the custom rendering
  anymore.
- **Migrate to the new path** — find where upstream moved the
  template (changelog, `templates list`) and rename your override
  to match.

### Pattern C — old layout (pre-1.0)

```
$ crap-cms templates layout
Old layout detected (3 files):
  static/list-toolbar.css → static/styles/parts/lists.css
  ...
```

You're on the pre-1.0 reshuffle layout. Aliases serve your old
paths transparently for this release, with deprecation warnings on
first hit. To clear the warnings, follow the auto-generated
migration recipe printed by `templates layout`.

See [Migrating from the old layout](../upgrade/migrating-from-old-layout.md)
for the full path map and recipe.

### Pattern D — context-key removal

A subtle case: your overlay template references a context key that
upstream removed. `templates status` reports the file as `behind`
but `templates diff` only shows the version-header change — the
silent breakage is in your template's `{{some.removed.key}}`
which now renders empty.

The fix: read the [template-context reference](../reference/template-context.md)
for the page you're overriding. Every page's typed context is
documented; if a key disappeared, you'll see it isn't listed.

A future `crap-cms doctor` command will scan your overlays for
references to removed context keys and flag them. Until then,
manual review against the template-context reference is the
defense.

## Releases that broke things

The pre-1.0 reshuffle (this release) is the largest layout change
crap-cms has done. From 1.0 onwards, breakage of this magnitude is
governed by the [stability tiers](../upgrade/stability-tiers.md):
`stable` modules get a deprecation cycle; `experimental` modules
can break in any minor; `internal` modules can break without
warning.

Subscribe to the `crap-cms` GitHub releases to see the changelog
under each release's **"Public surface"** heading — that's where
breaking changes get announced.

## Tooling gaps to be aware of

- **No automatic re-porter.** `templates extract --force` overwrites
  your edits with the new upstream. You re-apply your
  customizations manually. This is intentional — automatic merging
  silently breaks edge cases that only surface at runtime on pages
  you don't routinely test. Manual is honest.
- **No deep-import scanner yet.** If your overlay JS file `import`s
  an upstream helper that was renamed, the only error is a 404 in
  the browser console. The planned `crap-cms doctor` command will
  scan for these. Until then, exercise the admin in dev-mode after
  every upgrade and watch the console.
- **No auto-rebuild trigger.** After dropping or editing a file,
  the next request reads it (in dev mode) or restart picks it up
  (in production). No file-watcher.

## Verifying an upgrade went clean

After re-porting all `behind` and `orphaned` overrides:

```
$ crap-cms templates status

Templates customization status (config dir: ./crap, running version: 0.1.0-alpha.9)

  ✓ static/styles/themes/themes-acme.css   —  current
  ✓ templates/layout/base.hbs              —  current
  · templates/slots/dashboard_widgets/weather.hbs  —  user-original (no upstream counterpart)
  · static/components/custom.js            —  user-original (no upstream counterpart)

Summary: 2 current, 0 behind, 0 ahead, 0 pristine, 0 unknown header, 0 no header, 0 orphaned, 2 user-original
```

Zero `behind`, zero `orphaned`, zero `unknown header`. Restart, run
your manual smoke test, ship.
