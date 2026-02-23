# Template Overlay

The admin UI uses a template overlay system: templates in your config directory override the compiled defaults.

## How It Works

When rendering a template, Crap CMS checks:

1. **Config directory** — `<config_dir>/templates/<name>.hbs`
2. **Compiled defaults** — built into the binary at compile time

If a file exists in the config directory, it's used. Otherwise, the compiled default is used.

## Dev Mode

When `admin.dev_mode = true` in `crap.toml`, templates are reloaded from disk on every request. This enables live editing without restarting.

When `dev_mode = false`, templates are cached after first load (production mode).

## Template Inheritance

Templates use Handlebars partial inheritance:

```handlebars
{{#> layout/base}}
    <h1>My Custom Page</h1>
    <p>Content here</p>
{{/layout/base}}
```

The `layout/base` partial provides the HTML shell (head, sidebar, header).

## Field Partials

Each field type has a partial in `templates/fields/`:

- `fields/text.hbs`
- `fields/number.hbs`
- `fields/textarea.hbs`
- `fields/richtext.hbs`
- `fields/select.hbs`
- `fields/checkbox.hbs`
- `fields/date.hbs`
- `fields/email.hbs`
- `fields/json.hbs`
- `fields/relationship.hbs`
- `fields/array.hbs`

The edit form iterates field definitions and renders the matching partial.

## Overriding Templates

To customize a template, create the same file path under your config directory's `templates/` folder:

```
my-project/
└── templates/
    └── fields/
        └── richtext.hbs   # overrides the default richtext field template
```

## Available Template Variables

Templates receive context data from the Axum handlers. Common variables include:

- `collection` — collection definition
- `document` — document data (edit forms)
- `documents` — document list (list views)
- `fields` — field definitions
- `globals` — global definitions
- `user` — authenticated user (if auth is configured)
