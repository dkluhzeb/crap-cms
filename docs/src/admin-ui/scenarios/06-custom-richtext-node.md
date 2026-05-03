# Scenario 6: Add a custom richtext node

**Goal**: add a new richtext node type (e.g., a "Call to Action"
button or an `@mention` pill) that authors can insert inside any
richtext field in any collection.

**Difficulty**: medium. ~30 minutes from scratch to a working
custom node, with admin UI, validation, and HTML rendering.

**You'll touch**: `init.lua` only — no new files, no Rust, no JS.
Or run `crap-cms make node <name>` to scaffold the registration
file.

> Looking to add a **top-level custom field type** (e.g. a rating
> column on a collection, with stars in the admin and integer
> storage)? See [Scenario 7: Add a custom field type](07-custom-field-type.md).
> That scenario covers per-field render templates via
> `admin.template` + `admin.extra` — first-class shipped support.

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
