# Slots

Slots are **named extension points** in built-in templates where you
can drop your own content without overriding the surrounding
template. Where partials let you *replace* a markup chunk, slots let
you *add to* one.

```hbs
{{!-- Built-in template declares a slot --}}
{{slot "dashboard_widgets"}}
```

```hbs
{{!-- You drop a file at the matching path --}}
{{!-- <config_dir>/templates/slots/dashboard_widgets/weather.hbs --}}
<crap-card>
  <h3>Weather</h3>
  <p>{{temp}}°C</p>
</crap-card>
```

That's the entire mechanism. Drop a file under
`templates/slots/<slot_name>/<anything>.hbs`; it renders at the
matching `{{slot "..."}}` site in the upstream template, alongside
any other contributions to the same slot.

## How slot rendering works

When a built-in template hits `{{slot "name"}}`, the helper:

1. Enumerates every registered template name starting with
   `slots/name/` (both upstream defaults and your overlay files).
2. Sorts them alphabetically.
3. Renders each against the current page context.
4. Concatenates the output and writes it at the slot site.

If no slot files exist for that name, the helper renders **nothing**
— or, if the slot was declared in block form, the inline fallback.

## File naming

The filename inside `slots/<slot_name>/` doesn't matter for routing
— **anything** with a `.hbs` extension renders. The filename only
controls **render order** (alphabetical).

Use the filename to namespace your contributions and control order:

```
templates/slots/dashboard_widgets/
  10-weather.hbs           # renders first
  20-recent-activity.hbs   # renders second
  90-system-status.hbs     # renders last
```

A common convention is `NN-<purpose>.hbs` where `NN` is a two-digit
ordinal — leaves room for inserting future slots without renaming.

## Page context

Slot files render with the **same template context as the page they
appear in**. If you drop a file at
`slots/page_header_actions/admin-status.hbs`, it has access to
everything the surrounding `header.hbs` sees: `{{user.email}}`,
`{{nav.collections}}`, `{{crap.site_name}}`, and so on.

Reach past any hash-param overlay (see below) via `{{@root.x}}`:

```hbs
{{!-- slot file with hash params merged on top --}}
{{name}}                  {{!-- hash param --}}
{{@root.user.email}}      {{!-- always the page-level data --}}
```

## Hash params (per-invocation data)

Slots can pass per-invocation data the slot file sees at the root:

```hbs
{{!-- Built-in template invokes the slot with hash params --}}
{{slot "field_help" name=field.name kind=field.field_type}}
```

```hbs
{{!-- slot file sees the values at the root --}}
{{!-- slots/field_help/long-text-hint.hbs --}}
{{#if (eq kind "richtext")}}
  <p class="form__help">Use <kbd>Ctrl+B</kbd> for bold.</p>
{{/if}}
```

Hash params are useful for slots that fire **once per item** (e.g.
once per field, once per collection row) and need the item's data
without traversing the page context.

## Block form (inline fallback)

A built-in template can declare a slot with an inline fallback that
renders **only when no slot files exist**:

```hbs
{{#slot "dashboard_widgets"}}
  <p class="muted">No widgets configured yet.</p>
{{/slot}}
```

If you drop *any* file at `slots/dashboard_widgets/`, the fallback is
suppressed and your file(s) render instead.

## Built-in slots

These slots are declared in the built-in templates. Each one is a
public extension surface — its name and context are part of the
stable API.

| Slot | Declared in | Context | Use for |
|---|---|---|---|
| `head_extras` | `layout/base.hbs` | full page context | extra `<meta>` tags, OG tags, robots directives, PWA `<link rel="manifest">`, custom `<link rel="preconnect">`, analytics `<script>` |
| `body_end_scripts` | `layout/base.hbs` | full page context | end-of-body analytics, custom event listeners, third-party scripts that should load after the admin |
| `page_header_actions` | `layout/header.hbs` | full page context | extra buttons in the top header bar (next to the logout button) |
| `dashboard_widgets` | `dashboard/index.hbs` | dashboard context | custom dashboard cards (recent activity, system status, weather, queue depth, …) |
| `collection_edit_toolbar` | `collections/edit_form.hbs` | edit-form context (`document`, `collection`, `user`) | extra toolbar actions on collection edit pages (e.g., a "Preview" button) |
| `collection_edit_sidebar` | `collections/edit_sidebar.hbs` | edit-form context | extra sidebar panels on collection edit pages (related items, audit log, custom metadata) |
| `sidebar_bottom` | `layout/sidebar.hbs` | nav context | extra navigation links pinned to the bottom of the left sidebar |
| `login_extras` | `auth/login.hbs` | minimal auth context | additional content on the login page (compliance notices, SSO links, banner messages) |

## Worked example — dashboard weather widget

Goal: add a weather card to the admin dashboard that shows the
current temperature.

**Step 1**: drop a slot file at the matching path. Use
`{{data "name"}}` to pull dynamic values from a Lua function
registered below:

```hbs
{{!-- <config_dir>/templates/slots/dashboard_widgets/weather.hbs --}}
<div class="card">
  <div class="card__header">
    <span class="material-symbols-outlined">cloud</span>
    <h3>Weather</h3>
  </div>
  <div class="card__body">
    {{#with (data "weather_now")}}
      <p class="metric">{{temp}}°C, {{condition}}</p>
      <p class="muted">{{location}} — updated {{updated_at}}</p>
    {{else}}
      <p class="muted">Weather data unavailable.</p>
    {{/with}}
  </div>
</div>
```

**Step 2**: register the data function in `init.lua`:

```lua
-- <config_dir>/init.lua
crap.template_data.register("weather_now", function(ctx)
  -- ctx is the page render context. Return a table; {{#with (data ...)}}
  -- binds it. Return nil to fall through to the {{else}} branch.
  return {
    temp = 22,
    condition = "Sunny",
    location = "Berlin",
    updated_at = os.date("%H:%M"),
  }
end)
```

The function runs **on demand** — only when a page actually evaluates
`{{data "weather_now"}}`. Pages without your widget pay no cost.

**Step 3**: restart crap-cms (Lua loads at startup). Open
`/admin` — the card renders alongside any other dashboard widgets.

For the full version with `crap.http.request`, env-var-keyed API
auth, error handling, and caching, see
[Scenario 4: Dashboard widget](../scenarios/04-dashboard-widget.md).

## Adding a slot to a custom template

If you've overridden a built-in template (`<config_dir>/templates/foo.hbs`),
you can declare your own slots in it:

```hbs
{{!-- your overridden template --}}
<div class="my-page">
  {{slot "my_page_extras" page="my-page"}}
</div>
```

Anyone (including future-you) can extend the page by dropping a file
at `slots/my_page_extras/<anything>.hbs`. The hash param `page` is
visible inside slot files as `{{page}}` for context-sensitive output.

## When NOT to use a slot

- **Replacing a single value** (site name, theme color) — use a
  config field with `{{crap.foo}}` rendering.
- **Replacing markup** (the logo SVG, the meta-tags block) — use a
  partial drop at `templates/partials/<name>.hbs`.
- **Replacing a whole page template** — drop the file at
  `templates/<page>.hbs`.

Slots are specifically for **additive insertions** at named extension
points. Reaching for one when you really want a replacement leads to
contortions; reaching for a replacement when you want to add content
alongside upstream's defaults forces you to maintain a fork. See the
[admin UI overview](../index.md#when-to-use-what) for the full
mechanism matrix.
