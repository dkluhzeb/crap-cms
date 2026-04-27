# Template Formatter

Crap CMS ships a Handlebars formatter as a `crap-cms fmt` subcommand.
Templates in your config-dir overlay and the built-in `templates/`
directory are kept consistent the same way `cargo fmt` keeps Rust
consistent — one rule set, one tool, no editor disagreement.

```bash
crap-cms fmt                       # format everything under templates/
crap-cms fmt --check               # CI gate: exit 1 if any file would change
crap-cms fmt path/to/file.hbs      # one file
cat my.hbs | crap-cms fmt --stdio  # editor pipe
```

The formatter is **idempotent**: `fmt(fmt(x)) == fmt(x)`. Running it
twice produces the same result as running it once.

See [the CLI reference](../cli/flags.md#fmt--format-handlebars-templates)
for full flag documentation and editor integration snippets.

## Rule set

### 1. Indentation

Two-space indent. Both HTML elements **and** Handlebars block helpers
contribute one level each.

```hbs
{{#if user}}
  <div class="profile">
    <h1>{{user.name}}</h1>
  </div>
{{/if}}
```

`{{else}}` / `{{else if}}` returns to the parent level for the keyword,
then the body re-indents:

```hbs
{{#if (eq variant "fieldset")}}
  <fieldset>...</fieldset>
{{else if (eq variant "checkbox")}}
  <div class="checkbox">...</div>
{{else}}
  <label>...</label>
{{/if}}
```

`{{#> partial}}…{{/partial}}` invocations indent their body the same
way:

```hbs
{{#> partials/field}}
  <input type="text" name="email" />
{{/partials/field}}
```

### 2. Block helpers always own a line

`{{#if}}` / `{{#each}}` / `{{#unless}}` / `{{#with}}` / `{{else}}` /
`{{/x}}` each appear on their own line. Inline collapsing of
`{{#if x}}<span>...{{/if}}` is never produced — predictable beats
clever.

### 3. HTML attributes

- **Zero or one attribute** — kept inline:
  ```hbs
  <p>x</p>
  <a href="/x">y</a>
  ```
- **Two or more attributes** — each on its own line, closing `>` on its
  own line at the tag's column:
  ```hbs
  <a
    class="card"
    href="/admin/collections/{{slug}}"
    hx-get="/admin/collections/{{slug}}"
    hx-target="body"
    hx-push-url="true"
  >
    ...
  </a>
  ```

Embedded `{{#if x}}required{{/if}}` chunks within an attribute list are
preserved in place and stack as their own line in the multi-attr form.

### 4. Inline body collapse

A tag with **0–1 plain attributes** wrapping a single short
text-or-expression body stays on one line, as long as it fits within
100 characters:

```hbs
<span class="badge">{{t "draft"}}</span>
```

A nested element or block helper in the body forces multi-line:

```hbs
<p class="x">
  <span>y</span>
</p>
```

### 5. Mustache spacing

Compact: `{{t label}}`, not `{{ t label }}`. Sub-expressions are
unchanged: `{{t (concat "x" y)}}`.

### 6. Self-closing void elements

`<input />`, `<meta />`, `<br />`, `<hr />`, `<img />`, `<link />`,
`<source />`, `<area />`, `<embed />`, `<track />`, `<wbr />`,
`<col />`, `<base />`. The trailing ` />` is added if missing.

### 7. Lowercase tags + attrs, double-quoted values

```hbs
<INPUT TYPE='text'>           →   <input type="text" />
```

Values containing a literal `"` (e.g. inline JSON) fall back to single
quotes:

```hbs
<meta name="htmx-config" content='{"includeIndicatorStyles":false}' />
```

### 8. Boolean attributes are bare

```hbs
<input required="" />          →   <input required />
<input required="required" />  →   <input required />
<input required />             →   <input required />
```

A meaningful value on a known boolean attribute is **kept** (the same
attribute name can carry data on custom elements):

```hbs
<crap-relationship-search selected='{{json items}}' />
```

### 9. Comments preserved verbatim

Both `<!-- ... -->` and `{{!-- ... --}}` keep their internal whitespace,
line breaks, and structure. Documentation comments at the top of
partials survive the formatter unchanged:

```hbs
{{!-- partials/field
     Wraps a form field with label + required marker + locale badge +
     error + help text. Three structural variants share the same
     error/help/required logic.
     ...
     --}}
```

### 10. Triple-stash passthrough

`{{{render_field this}}}` is treated like a regular expression — same
spacing rules, no escaping.

### 11. Whitespace + final newline

- Two or more consecutive blank lines collapse to one.
- Trailing whitespace stripped per line.
- Single final newline guaranteed.

## What's intentionally **not** done

- **Text content reflow.** Prose inside text nodes is preserved as-is,
  not wrapped at any column.
- **Embedded `<script>` and `<style>` formatting.** Their bodies are
  passed through verbatim. Standalone JS/CSS files are formatted by
  Biome.
- **Whitespace-control mustaches** (`{{~ ... ~}}`). Rare; passed
  through unchanged.
- **Complex expression re-flow inside `{{...}}`.** Only outer-delimiter
  spacing is normalised.

## Editor integration

### Neovim (conform.nvim)

```lua
{
  'stevearc/conform.nvim',
  init = function()
    vim.filetype.add { extension = { hbs = 'handlebars' } }
  end,
  opts = {
    formatters_by_ft = { handlebars = { 'crap_cms' } },
    formatters = {
      crap_cms = {
        command = 'crap-cms',
        args = { 'fmt', '--stdio' },
        stdin = true,
      },
    },
  },
}
```

### Pre-commit hook

```bash
#!/bin/bash
set -e
cargo fmt -- --check
cargo clippy -- -D warnings
cargo run --quiet --bin crap-cms -- fmt --check
```

### CI

```yaml
- name: Handlebars template formatting
  run: cargo run --quiet --bin crap-cms -- fmt --check
```
