# Scenario 6: Add a custom content block

**Goal**: add a new content block type (e.g., a "Call to Action"
button or an `@mention` pill) that authors can insert inside any
richtext field in any collection.

**Difficulty**: medium. ~30 minutes from scratch to a working
custom block, with admin UI, validation, and HTML rendering.

**You'll touch**: `init.lua` only — no new files, no Rust, no JS.

> **Looking to add a top-level custom field type** (e.g. a `rating`
> column shaped like a 1–5 integer with star UI everywhere `rating`
> fields appear)? That's a different problem — top-level
> Lua-registered field types are a **deferred roadmap item**, and
> the workarounds available today are listed in the [What about a
> wholly new top-level field type?](#what-about-a-wholly-new-top-level-field-type)
> section at the bottom of this page. Custom *content blocks* (this
> scenario) are the closest shipped equivalent: typed data with full
> admin UI editing, scoped to live inside richtext content.

## Approach

Crap CMS lets you register **custom richtext nodes** from Lua via
`crap.richtext.register_node`. A node is a typed insertion point
inside the richtext document tree:

- **Block-level** (`inline = false`) — a full block, like a hero
  card or a CTA button row.
- **Inline** (`inline = true`) — sits inside a paragraph, like an
  `@mention` pill or a custom badge.

Each node carries its own typed attributes (text, number, select,
date, etc.), full admin UI editing, validation, and a Lua render
function that turns the node into HTML for output.

The `example/init.lua` shipped in the repo defines two real custom
nodes — a CTA button and an `@mention` pill — that you can copy as
a starting template.

## Step 1 — register the node

Add to `<config_dir>/init.lua`:

```lua
-- Block-level: Call to Action button
crap.richtext.register_node("cta", {
  label = "Call to Action",
  inline = false,
  attrs = {
    crap.fields.text({
      name = "text",
      required = true,
      min_length = 2,
      max_length = 80,
      admin = {
        label = "Button Text",
        description = "The visible text on the button",
      },
    }),
    crap.fields.text({
      name = "url",
      required = true,
      admin = { label = "URL", placeholder = "https://..." },
    }),
    crap.fields.select({
      name = "style",
      admin = { label = "Style" },
      options = {
        { label = "Primary", value = "primary" },
        { label = "Secondary", value = "secondary" },
        { label = "Outline", value = "outline" },
      },
    }),
    crap.fields.number({
      name = "padding",
      min = 0,
      max = 100,
      admin = { label = "Padding (px)", step = "1", width = "50%" },
    }),
  },
  searchable_attrs = { "text" },
  render = function(attrs)
    local style = ""
    if attrs.padding and attrs.padding ~= "" then
      style = string.format(' style="padding: %spx 0"', tostring(attrs.padding))
    end
    return string.format(
      '<a href="%s" class="btn btn--%s"%s>%s</a>',
      attrs.url or "#",
      attrs.style or "primary",
      style,
      attrs.text or ""
    )
  end,
})
```

What each spec field does:

| Field | Required | Effect |
|---|---|---|
| `label` | no | Display name in the richtext block picker. Defaults to the node name. |
| `inline` | no | `true` for inline nodes (mentions, badges); `false` (default) for block-level. |
| `attrs` | no | Array of typed attributes via `crap.fields.text/number/select/date/email/json/code/textarea/radio/checkbox`. Restricted to scalar types — no nested arrays/blocks. |
| `searchable_attrs` | no | Names of attrs whose values get indexed for full-text search. Must reference real attr names. |
| `render` | no | Lua function `(attrs) -> string` that turns the node into HTML. Return any HTML you want; remember to escape user input. **If omitted, the node falls through to the [`<crap-node>` passthrough](#what-renders-without-a-render-function) instead of returning HTML directly.** |

## Two ways to render custom-node output

Custom richtext nodes have **two output paths**, and there's no
HBS-template equivalent — node output is programmatic.

### Path A — Lua `render` function (server-side HTML)

Provide a `render = function(attrs) ... end` in the spec. When the
admin (or anything else) calls `crap.richtext.render(content)`, your
function fires for every node of that type and its return value is
spliced into the HTML output. This is the path used by the CTA
example above.

Best for:
- Pages that render the full HTML server-side (Markdown blogs,
  static sites, server-rendered pages backed by crap-cms).
- Cases where you want the `<a class="btn">` (or whatever) to
  appear directly in the served HTML.

### Path B — `<crap-node>` passthrough (client-side rendering)

Omit `render` entirely. The renderer emits a placeholder element
instead:

```html
<crap-node data-type="cta" data-attrs='{"text":"Sign up","url":"/signup",...}'></crap-node>
```

Your downstream consumer (a Web Component, a JS framework, a JSX
component, an iOS app reading the JSON) picks up the
`<crap-node data-type="...">` element and renders it however it
wants. The attrs are JSON-encoded into the `data-attrs` attribute.

Best for:
- Single-page apps and React/Vue/Svelte frontends where the
  richtext is rendered by a frontend component, not the server.
- Cases where the render is environment-specific (different markup
  on iOS vs. web, dark mode adjustments at view time, A/B-tested
  variants, etc.).
- Mixed deployments: server-render the framing, let the client
  decide how to render the richtext nodes.

You can register a node with **just `attrs`** — admin-UI editing
works fully (the form is auto-generated from the attrs spec), and
the consuming side handles rendering:

```lua
crap.richtext.register_node("cta", {
  label = "Call to Action",
  attrs = {
    crap.fields.text({ name = "text", required = true }),
    crap.fields.text({ name = "url", required = true }),
    crap.fields.select({ name = "style", options = { ... } }),
  },
  -- No render fn — output will be a <crap-node> passthrough.
})
```

### What renders *without* a render function

When the renderer encounters a custom node and no `render` is
registered for it, output looks like:

```html
<crap-node
  data-type="cta"
  data-attrs='{"text":"Sign up","url":"/signup","style":"primary"}'
></crap-node>
```

The consumer is responsible for transforming these elements into
final HTML. The `data-attrs` is HTML-escaped JSON of the node's
attribute values.

### Why no HBS template path

Crap-cms doesn't ship an HBS template renderer for custom richtext
nodes. The two paths above (Lua function + JSON passthrough) cover
both server-side and client-side rendering needs without
introducing a third mechanism. If you specifically want HBS,
you can call back into Handlebars from your Lua `render` function
— but in practice the Lua `string.format` / `table.concat` style
shown in the CTA example is sufficient for node-level HTML.

## Admin form rendering

The admin's in-document editing form (the modal that pops up when
you insert or edit a node) is **auto-generated from the `attrs`
spec**. There's no per-node admin template to override; the form
is built dynamically by `<crap-richtext>` from the attr definitions.

This is by design — node attrs are restricted to scalar types
(text, number, select, etc.), so the auto-generated form is always
a flat collection of standard `<crap-*>` field components. The
consistency means every custom node gets the same UX as the
built-in nodes.

If you need a wholly different admin UI for a node (e.g., a custom
visual picker, drag-and-drop sub-editor, etc.), you'd need to fork
`<crap-richtext>` itself. For most "extra typed data on a
content node" cases, the auto-form is exactly what you want.

## Step 2 — restart

Lua loads at startup. Restart crap-cms.

## Step 3 — use it

Open any collection with a richtext field (e.g., `posts.body`).
Click the block-picker `+` button — your "Call to Action" entry
appears alongside the built-in nodes (paragraph, heading,
blockquote, etc.). Insert one; the admin renders an editable form
for `text`, `url`, `style`, `padding` based on the `attrs` you
declared.

When the document is rendered (server-side, via the richtext field's
`{{render}}` helper), your `render` function fires for every CTA
node and returns the HTML you specified.

## Inline nodes

For inline content (mentions, badges, custom small widgets), set
`inline = true`:

```lua
crap.richtext.register_node("mention", {
  label = "Mention",
  inline = true,
  attrs = {
    crap.fields.text({ name = "name", required = true, admin = { label = "Name" } }),
    crap.fields.text({ name = "user_id", admin = { label = "User ID" } }),
  },
  searchable_attrs = { "name" },
  render = function(attrs)
    return string.format('<span class="mention">@%s</span>', attrs.name or "?")
  end,
})
```

Inline nodes appear in the inline-formatting toolbar (next to bold,
italic, link) instead of the block picker.

## What you can build with this

The block-shape is general enough to cover most "custom data with
admin UI inside a document" needs:

| Use case | Shape |
|---|---|
| Hero / banner | block-level node with text + image-URL + alignment select |
| Embed (YouTube, Twitter) | block-level node with `url` text attr; render emits the embed iframe |
| Pull quote | block-level node with `quote` textarea + `author` text |
| Footnote | inline node with `note` textarea |
| Mention | inline node with `user_id` lookup |
| Glossary term | inline node with `term` text + `definition` textarea |
| CTA / button | (the example above) |
| Code playground | block-level node with `lang` select + `code` code attr |

The `render` function gives you total control over output HTML, so
you can wrap nodes in any markup your site theme needs.

## Limitations

These are real today:

- **Attr types restricted to scalars.** Allowed: `text`, `number`,
  `textarea`, `select`, `radio`, `checkbox`, `date`, `email`,
  `json`, `code`. Not allowed: nested `array`, nested `blocks`,
  `relationship`. (Use `json` if you need a structured blob.)
- **No live preview of the rendered HTML.** The admin shows the
  attr-editing form; the rendered output is what end-users see on
  the public site. Test renders happen server-side at content
  fetch time.
- **`searchable_attrs` must be top-level scalars.** You can't
  search into a nested JSON attr.

## What about a wholly new top-level field type?

The above covers "custom data shapes inside richtext content." A
different problem is **adding a brand new top-level `FieldType`
variant** — say, a `rating` type that's a 1–5 integer with
star-rendering everywhere `rating` fields appear in any collection,
plus first-class sorting / filtering / SQL column-type support.

This currently requires a Rust change. `FieldType` is a hardcoded
enum in [`src/core/field/field_type.rs`](https://github.com/dkluhs/crap-cms/blob/main/src/core/field/field_type.rs),
and `FieldAdmin` is a fixed Rust struct (no arbitrary
`admin.component = "..."` extension key — additional keys are
rejected at parse time). **Top-level Lua-registered field types are
a tracked roadmap item, deferred from the pre-1.0 reshuffle.**

Until that lands, four workarounds get you most of the way for
common rating-style use cases — each with real tradeoffs:

### Workaround A — `number` field + per-field `admin.template` (recommended)

Declare `rating` as a `number` field with `min = 1`, `max = 5`,
and point it at a custom render template via `admin.template`:

```lua
crap.collections.define("products", {
  fields = {
    crap.fields.text({ name = "name", required = true }),
    crap.fields.number({
      name = "rating",
      min = 1,
      max = 5,
      admin = {
        template = "fields/rating",
        extra = {
          icon = "star",
          empty_icon = "star_outline",
          color = "amber",
        },
      },
    }),
  },
})
```

Drop the per-field template at
`<config_dir>/templates/fields/rating.hbs`:

```hbs
{{#> partials/field}}
  <crap-stars
    name="{{name}}"
    value="{{value}}"
    data-min="{{min}}"
    data-max="{{max}}"
    data-icon="{{extra.icon}}"
    data-empty-icon="{{extra.empty_icon}}"
    data-color="{{extra.color}}"
  ></crap-stars>
{{/partials/field}}
```

Register a `<crap-stars>` Web Component via `custom.js`:

```js
// <config_dir>/static/components/custom.js
import './rating.js';
```

```js
// <config_dir>/static/components/rating.js
class CrapStars extends HTMLElement {
  // ... render N clickable stars (where N = data-max), write value
  // back to a hidden input, fire crap:change for <crap-dirty-form>
  // integration.
}
customElements.define('crap-stars', CrapStars);
```

**How this works**:

- **`admin.template = "fields/rating"`** opts this *one* field
  out of the default `fields/number` lookup in `RenderFieldHelper`.
  Other `number` fields keep using `fields/number.hbs` — no global
  override, no field-name matching.
- **`admin.extra = {...}`** in the Lua schema becomes the flat
  top-level `extra` key on the render context. The template reads
  it as `{{extra.icon}}`, etc. Same template + JS component can be
  reused across fields with different settings (different colors,
  icons, swatches) without forking.
- **The data is still a `number`**: SQL column is `INTEGER`,
  validation uses `min`/`max`, sorting and filtering work
  natively.

**Path safety**: `admin.template` paths are validated at
field-parse time — only `[a-zA-Z0-9/_-]` allowed, no `..`, no
absolute paths. A bad path is rejected at startup with a clear
error.

**Drift tracking**: your custom template lives at
`<config_dir>/templates/fields/rating.hbs` — it's user-original
(no upstream counterpart at that path), so `crap-cms templates
status` flags it as `· user-original`. Nothing to drift against.

### Workaround B — `select` field + custom rendering

Declare as a `select` with five options:

```lua
crap.fields.select({
  name = "rating",
  options = {
    { value = "1", label = "★" },
    { value = "2", label = "★★" },
    { value = "3", label = "★★★" },
    { value = "4", label = "★★★★" },
    { value = "5", label = "★★★★★" },
  },
})
```

Stores as a string. No template override needed — the built-in
`select` rendering shows the labels in a dropdown. **Tradeoffs**:
no star *click* UI (just a dropdown of star strings), and the
stored value is a string, not an integer (so SQL sorting orders
lexicographically, not numerically — workable for 1–5 but breaks if
you scale to 1–10).

### Workaround C — `json` field + custom Web Component

Declare as `json`:

```lua
crap.fields.json({ name = "rating" })
```

The data is opaque to crap-cms. Override
`templates/fields/json.hbs` (globally) to insert your `<crap-stars>`
component when the field name matches:

```hbs
{{#if (eq name "rating")}}
  <crap-stars ...></crap-stars>
{{else}}
  {{!-- original json textarea --}}
{{/if}}
```

**Tradeoffs**: no built-in validation (it's whatever JSON shape you
write; you're responsible for shape-checking). No SQL-level sorting
or filtering. Maximum flexibility for novel shapes; minimum
ergonomics for simple cases.

### Workaround D — custom richtext block

If the rating belongs **inside content** (a "Review" article body
with embedded ratings, not a top-level "rating" column on the
collection), use `crap.richtext.register_node` per this scenario.
That's the cleanest shipped path, with no fragility — but it scopes
the rating to richtext content, not row-level fields.

### When to write the Rust patch instead

If you genuinely need:
- A new SQL column type (e.g., a `rating` column distinct from
  `INTEGER`) with custom migration handling,
- Custom validation rules at the field-type level (not workable as
  a `before_change` hook),
- The field type to ship as part of a published collection schema
  that other crap-cms instances should accept,

…then a Rust patch to add the `FieldType` variant is the honest
answer today. Track [the Phase 4b roadmap entry](../upgrade/migrating-from-old-layout.md)
for when Lua-registered field types unblock this without a fork.

## Verifying

```
$ crap-cms templates status
  · init.lua  —  user-original (no upstream counterpart)
```

Custom-node registrations live entirely in `init.lua`. They're
user-original and never drift.

The shipped example has the CTA + mention nodes working — see
[`example/init.lua`](https://github.com/dkluhs/crap-cms/blob/main/example/init.lua)
for the live reference.
