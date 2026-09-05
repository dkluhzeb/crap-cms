# Command-Line Reference

```
crap-cms <COMMAND> [OPTIONS]
```

Use `crap-cms --help` to list all commands, or `crap-cms <command> --help` for details on a specific command.

## Global Flags

| Flag | Description |
|------|-------------|
| `-C`, `--config <PATH>` | Path to the config directory (overrides auto-detection) |
| `-V`, `--version` | Print version and exit |
| `-h`, `--help` | Print help |

## Config Directory Resolution

Most commands need a config directory (the folder containing `crap.toml`). The CLI resolves it in this order:

1. **`--config` / `-C` flag** — explicit path, highest priority
2. **`CRAP_CONFIG_DIR` environment variable** — useful for CI/Docker
3. **Auto-detection** — walks up from the current working directory looking for `crap.toml`

If you `cd` into your project directory (or any subdirectory), commands just work without any flags:

```bash
cd my-project
crap-cms serve
crap-cms status
crap-cms user list
```

From elsewhere, use `-C`:

```bash
crap-cms -C ./my-project serve
crap-cms -C ./my-project status
```

Or set the environment variable:

```bash
export CRAP_CONFIG_DIR=./my-project
crap-cms serve
```

## Commands

### `serve` — Start the server

```bash
crap-cms serve [-d] [--stop] [--restart] [--status] [--json] [--only <admin|grpc>] [--no-scheduler]
```

| Flag | Description |
|------|-------------|
| `-d`, `--detach` | Run in the background (prints PID and exits) |
| `--stop` | Stop a running detached instance (SIGTERM, then SIGKILL after 10s) |
| `--restart` | Restart a running detached instance (stop + start) |
| `--status` | Show whether a detached instance is running (PID, uptime) |
| `--json` | Output logs as structured JSON (for log aggregation; forwarded to the detached child) |
| `--only <admin\|grpc>` | Start only the specified server. Omit to start both. `api` is accepted as a backward-compatible alias for `grpc`. |
| `--no-scheduler` | Disable the background job scheduler |

`--detach`, `--stop`, `--restart`, and `--status` are mutually exclusive.

```bash
crap-cms serve                    # foreground
crap-cms serve -d                 # detached (background)
crap-cms serve --status           # is it running?
crap-cms serve --stop             # stop detached instance
crap-cms serve --restart          # stop + start detached
crap-cms serve --json
crap-cms serve --only admin       # admin UI only
crap-cms serve --only grpc        # gRPC API only (`--only api` also works)
crap-cms serve --no-scheduler     # both servers, no scheduler
crap-cms serve --only admin --no-scheduler
crap-cms serve -d --only grpc     # detached, API only
```

### `work` — Run a standalone job worker

```bash
crap-cms work [--detach] [--stop] [--restart] [--status] [--queues <list>] [--concurrency <n>] [--no-cron]
```

Runs a dedicated job worker without HTTP/gRPC servers. For multi-server deployments where app servers run `serve --no-scheduler` and dedicated workers process jobs.

| Flag | Description |
|------|-------------|
| `-d`, `--detach` | Run in the background |
| `--stop` | Stop a running detached worker |
| `--restart` | Restart a running detached worker |
| `--status` | Show whether a detached worker is running |
| `--queues <list>` | Comma-separated queue names to process (default: all) |
| `--concurrency <n>` | Override `jobs.max_concurrent` for this worker |
| `--no-cron` | Skip cron scheduling (let another worker handle it) |

```bash
crap-cms work                           # process all queues
crap-cms work --queues email            # email queue only
crap-cms work --queues heavy --concurrency 2  # heavy jobs, limited concurrency
crap-cms work --no-cron                 # skip cron, just process queued jobs
crap-cms work -d                        # detached
crap-cms work --status                  # check if running
crap-cms work --stop                    # stop detached worker
```

**Multi-server deployment:**
```bash
# App servers (no job processing)
crap-cms serve --no-scheduler

# Dedicated workers
crap-cms work -d                        # general worker
crap-cms work -d --queues email         # email-only worker
crap-cms work -d --queues heavy --concurrency 2  # heavy processing
```

### `status` — Show project status

```bash
crap-cms status [--check]
```

| Flag | Description |
|------|-------------|
| `--check` | Run best-practice health checks on configuration and project state |

Prints a comprehensive project overview:

- **Server config** — ports, compression, rate limiting
- **Database** — path, size (SQLite), or backend name (PostgreSQL)
- **Uploads** — total size and file count
- **Locales** — configured locales and fallback setting
- **Collections** — row counts, trash counts (soft-deleted documents), and tags (auth, upload, versions, soft_delete)
- **Globals** — registered global documents
- **Versioning** — which collections have drafts enabled and max version limits
- **Access rules** — read/create/update/delete functions per collection and global, with default deny/allow indicator
- **Hooks** — which lifecycle hooks are wired and to which functions
- **Live events** — event mode per target (metadata, full, disabled, or filter function)
- **Migrations** — total, applied, pending
- **Jobs** — defined, running, failed in last 24h

#### `status --check`

Runs a best-practice audit with 24 checks across four categories:

**Security:**
- Auth secret too short or placeholder value
- Brute-force protection disabled
- `default_deny = false` (collections publicly accessible)
- Collections without access rules
- gRPC rate limiting disabled with auth collections
- CORS wildcard origin with credentials

**Performance:**
- `max_depth > 3` (N+1 query growth)
- Cache disabled with relationship fields
- Pool size too small or connection timeout too aggressive
- Response compression disabled
- `pagination.max_limit > 500`
- Too many hooks or before_change hooks per collection
- Too many collections with `live_mode = "full"`

**Configuration:**
- `dev_mode` enabled
- `default_depth` exceeds `max_depth`
- Email provider set to `"log"` with `verify_email` enabled

**Operations:**
- Pending migrations
- Auth collection without soft_delete
- Upload collection without versioning
- Soft delete without retention policy
- Empty auth collection (0 users)

```bash
crap-cms status                # project overview
crap-cms status --check        # overview + health audit
```

`status --check` exits with code `2` when the audit finds warnings and
`0` when clean — usable as a CI gate.

### `bench` — Benchmark hooks, queries, and write cycles

Developer performance profiling tool. Measures hook execution time, query latency, and end-to-end write cycle duration.

#### `bench hooks`

```bash
crap-cms bench hooks [-c <COLLECTION>] [-n <ITERATIONS>] [--hooks <LIST>] [--exclude <LIST>] [--all] [-d <JSON>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | — | Filter to a specific collection |
| `--iterations` | `-n` | `10` | Number of iterations per hook |
| `--hooks` | — | — | Run only these hooks (comma-separated function refs) |
| `--exclude` | — | — | Run all hooks except these (comma-separated) |
| `--all` | — | — | Run all hooks (skip interactive selection) |
| `--data` | `-d` | — | Input data as JSON object |

**Safety model:** Hooks may have external side effects (webhooks, API calls). By default, an interactive `MultiSelect` wizard lets you choose which hooks to benchmark. Use `--hooks` or `--all` for non-interactive use.

**Data resolution:** `--data` JSON > existing document from DB > synthetic fallback. Hook errors are caught and reported without stopping the benchmark.

```bash
crap-cms bench hooks                                    # interactive wizard
crap-cms bench hooks --all                              # run all (with warning)
crap-cms bench hooks --hooks hooks.auto_slug -n 20      # specific hook, 20 iterations
crap-cms bench hooks -c posts --exclude hooks.send_webhook  # all posts hooks except one
```

#### `bench queries`

```bash
crap-cms bench queries [-c <COLLECTION>] [--explain] [-w <JSON>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | — | Filter to a specific collection |
| `--explain` | — | — | Show `EXPLAIN QUERY PLAN` output (SQLite only) |
| `--where` | `-w` | — | JSON filter clause (same format as gRPC `where` parameter) |

Read-only — no side effects, no confirmation needed. The `--where` filter uses the same JSON syntax as the gRPC API (e.g., `{"slug": {"equals": "my-post"}}`). Combined with `--explain`, this shows whether queries hit indexes.

```bash
crap-cms bench queries                                           # all collections
crap-cms bench queries -c posts --explain                        # single collection with query plan
crap-cms bench queries -c posts --where '{"status": "published"}' --explain  # filtered + plan
```

#### `bench create`

```bash
crap-cms bench create <COLLECTION> [-n <ITERATIONS>] [-d <JSON>] [--no-hooks] [-y]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--iterations` | `-n` | `5` | Number of iterations |
| `--data` | `-d` | — | Input data as JSON object |
| `--no-hooks` | — | — | Skip hooks (measure pure validation + persist) |
| `--yes` | `-y` | — | Skip confirmation prompt |

Runs the full service-layer create cycle (access check, validation, before-hooks, persist, after-hooks) inside a transaction that is rolled back after each iteration. **No data is persisted.**

When hooks are enabled and `-y` is not set, a confirmation prompt is shown because hooks may call external APIs. Unique fields are automatically randomized per iteration to avoid constraint violations.

```bash
crap-cms bench create posts                  # full cycle with confirmation
crap-cms bench create posts -y               # skip confirmation
crap-cms bench create posts --no-hooks       # pure validation + persist
crap-cms bench create posts -y -n 20         # 20 iterations, no prompt
crap-cms bench create posts -d '{"title": "test", "slug": "bench-test"}'  # custom data
```

### `user` — User management

#### `user create`

```bash
crap-cms user create [-c <COLLECTION>] [-e <EMAIL>] [-p <PASSWORD>] [-f <KEY=VALUE>]...
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | `users` | Auth collection slug |
| `--email` | `-e` | — | User email (prompted if omitted) |
| `--password` | `-p` | — | User password (prompted if omitted) |
| `--field` | `-f` | — | Extra fields as key=value (repeatable) |

```bash
# Interactive (prompts for password)
crap-cms user create -e admin@example.com

# Non-interactive
crap-cms user create \
    -e admin@example.com \
    -p secret123 \
    -f role=admin \
    -f name="Admin User"
```

#### `user list`

```bash
crap-cms user list [-c <COLLECTION>]
```

Lists all users with ID, email, locked status, and verified status (if email verification is enabled).

```bash
crap-cms user list
crap-cms user list -c admins
```

#### `user info`

```bash
crap-cms user info [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

Shows detailed info for a single user: ID, email, locked/verified status, password status, timestamps, and all field values.

```bash
crap-cms user info -e admin@example.com
crap-cms user info --id abc123
```

#### `user delete`

```bash
crap-cms user delete [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Auth collection slug (default: `users`) |
| `--email` | `-e` | User email |
| `--id` | — | User ID |
| `--confirm` | `-y` | Skip confirmation prompt |

#### `user lock` / `user unlock`

```bash
crap-cms user lock [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
crap-cms user unlock [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

#### `user reset-totp`

```
crap-cms user reset-totp [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-y]
```

Clears a user's TOTP enrollment (secret, confirmation, replay guard); they
re-enroll on their next login. Requires `mfa = "totp"` on the collection;
prompts for confirmation unless `-y` is passed.

```
```

#### `user verify` / `user unverify`

```bash
crap-cms user verify [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
crap-cms user unverify [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

Manually mark a user's email as verified or unverified. Only works on collections with `verify_email = true`. Useful when email is not configured.

#### `user change-password`

Change a user's password. Prompts for the new password if `-p` is omitted.

```bash
crap-cms user change-password [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-p <PASSWORD>]
```

### `init` — Scaffold a new config directory

```bash
crap-cms init [DIR] [--no-input]
```

Runs an interactive wizard that scaffolds a complete config directory. When no directory is given, the wizard prompts for a project path (default `./crap-cms`). With `--no-input` the wizard is skipped entirely — defaults are used, an auth (`users`) and an upload (`media`) collection are created, and **`DIR` is required** (there is no default in non-interactive mode).

The wizard prompts for:

| Prompt | Default | Description |
|--------|---------|-------------|
| Project path | `./crap-cms` | Target directory (only when `DIR` is omitted) |
| Admin port | `3000` | Port for the admin UI |
| gRPC port | `50051` | Port for the gRPC API |
| Enable localization? | No | If yes, prompts for default locale and additional locales |
| Default locale | `en` | Default locale code (only if localization enabled) |
| Additional locales | — | Comma-separated (e.g., `de,fr`) |
| Create auth collection? | Yes | Creates a `users` collection with email/password login |
| Create first admin user? | Yes | Prompts for email and password immediately |
| Create upload collection? | Yes | Creates a `media` collection for file/image uploads |
| Create another collection? | No | Repeat to add more collections interactively |

A 64-character auth secret is auto-generated and written to `crap.toml`. A `.mcp.json` file is also created for [Claude Code](../mcp/overview.md) integration.

```bash
crap-cms init ./my-project
```

After scaffolding:

```bash
cd my-project
crap-cms serve
```

### `make` — Generate scaffolding files

#### `make collection`

```bash
crap-cms make collection [SLUG] [-F <FIELDS>] [-T] [--auth] [--upload] [--versions] [--no-input] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--fields` | `-F` | Inline field shorthand (see below) |
| `--no-timestamps` | `-T` | Set `timestamps = false` |
| `--auth` | — | Enable auth (email/password login) |
| `--upload` | — | Enable uploads (file upload collection) |
| `--versions` | — | Enable versioning (draft/publish workflow) |
| `--no-input` | — | Non-interactive mode — skip all prompts, use flags and defaults only |
| `--force` | `-f` | Overwrite existing file |

Without `--no-input`, missing arguments (slug, fields) are collected via interactive prompts. The field survey asks for name, type, required, and localized (if [localization is enabled](../locale/overview.md)).

**Field shorthand syntax:**

```
name:type[:modifier][:modifier]...
```

Modifiers are order-independent:

| Modifier | Description |
|----------|-------------|
| `required` | Field is required |
| `localized` | Field has per-locale values (see [Localization](../locale/overview.md)) |

Container types take their sub-fields in parentheses directly after the type (modifiers follow the closing `)`):

| Type | Syntax |
|------|--------|
| `group`, `array`, `row`, `collapsible` | `name:type(subfields):modifiers` — e.g. `seo:group(title:text,description:textarea)` |
| `blocks` | `name:blocks(type\|label(subfields),...)` — e.g. `content:blocks(hero\|Hero(heading:text:required),cta\|Call to Action(url:text))` |
| `tabs` | `name:tabs(label(subfields),...)` — e.g. `settings:tabs(Content(title:text),Style(variant:select))` |

Nesting is unlimited; commas and colons inside parentheses belong to the inner field list. Any other type given sub-fields is an error.

```bash
# Basic
crap-cms make collection posts

# With fields
crap-cms make collection articles \
    -F "title:text:required,body:richtext"

# With localized fields
crap-cms make collection pages \
    -F "title:text:required:localized,body:textarea:localized,slug:text:required"

# Auth collection
crap-cms make collection users --auth

# Upload collection
crap-cms make collection media --upload

# Non-interactive with versions
crap-cms make collection posts \
    -F "title:text:required,body:richtext" --versions --no-input
```

#### `make global`

```bash
crap-cms make global [SLUG] [-F <FIELDS>] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--fields` | `-F` | Inline field shorthand (same syntax as `make collection`) |
| `--force` | `-f` | Overwrite existing file |

```bash
crap-cms make global site_settings
crap-cms make global nav -F "links:array(label:text:required,url:text)"
```

#### `make hook`

```bash
crap-cms make hook [NAME] [-t <TYPE>] [-c <COLLECTION>] [-l <POSITION>] [-F <FIELD>] [--force]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--type` | `-t` | Hook type: `collection`, `field`, `access`, or `condition` |
| `--collection` | `-c` | Target collection or global slug |
| `--position` | `-l` | Lifecycle position (e.g., `before_change`, `after_read`) |
| `--field` | `-F` | Target field name (field hooks only; watched field for condition hooks) |
| `--force` | — | Overwrite existing file |

Missing flags are resolved via interactive prompts. The wizard lists collections and globals from the registry (globals are tagged). For non-interactive mode, the slug is auto-detected as a global if it exists in the globals registry.

**Valid positions by type:**

| Type | Positions |
|------|-----------|
| `collection` | `before_validate`, `before_change`, `after_change`, `before_read`, `after_read`, `before_delete`, `after_delete`, `before_broadcast` |
| `field` | `before_validate`, `before_change`, `after_change`, `after_read` |
| `access` | `read`, `create`, `update`, `delete`, `trash`, `draft`, `versions`, `unlock`, `admin`, `mcp` (globals: only `read`, `draft`, `update`, `versions`, `admin`, `mcp`) |
| `condition` | `table`, `boolean` |

Generated hooks use per-collection typed annotations for IDE support:

- **Collection hooks:** `crap.hook.Posts`, `crap.hook.global_site_settings`
- **Field hooks:** `crap.field_hook.Posts`, `crap.field_hook.global_site_settings`
- **Condition hooks:** `crap.data.Posts`, `crap.global_data.SiteSettings`
- **Delete hooks:** generic `crap.HookContext` (data only contains the document ID)
- **Access hooks:** generic `crap.AccessContext`

```bash
# Interactive (prompts for everything)
crap-cms make hook

# Fully specified
crap-cms make hook auto_slug \
    -t collection -c posts -l before_change

# Field hook
crap-cms make hook normalize_email \
    -t field -c users -l before_validate -F email

# Access hook
crap-cms make hook owner_only \
    -t access -c posts -l read

# Condition hook (client-side table)
crap-cms make hook show_external_url \
    -t condition -c posts -l table -F post_type
```

#### `make job`

```bash
crap-cms make job [SLUG] [-s <SCHEDULE>] [-q <QUEUE>] [-r <RETRIES>] [-t <TIMEOUT>] [-f]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--schedule` | `-s` | — | Cron expression (e.g., `"0 3 * * *"`) |
| `--queue` | `-q` | `default` | Queue name |
| `--retries` | `-r` | *(queue default)* | Max retry attempts. Omit to let the job inherit `[jobs.queues.<queue>] retries` at runtime; pass an explicit value (including `0`) to write a fixed `retries` into the generated Lua |
| `--timeout` | `-t` | 60 | Timeout in seconds |
| `--force` | `-f` | — | Overwrite existing file |

```bash
# Interactive (prompts for slug)
crap-cms make job

# With schedule
crap-cms make job cleanup_expired -s "0 3 * * *" -r 3 -t 300

# Simple job (triggered from hooks)
crap-cms make job send_welcome_email
```

#### `make page`

```bash
crap-cms make page [SLUG] [-l <LABEL>] [-s <SECTION>] [-i <ICON>] [-a <ACCESS>] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--label` | `-l` | Sidebar label (defaults to the title-cased slug) |
| `--section` | `-s` | Sidebar section heading (e.g. `"Tools"`) |
| `--icon` | `-i` | Material Symbols icon name |
| `--access` | `-a` | Lua hook ref for access control (e.g. `access.admin_only`) |
| `--force` | `-f` | Overwrite existing file |

Writes `templates/pages/<slug>.hbs` (served at `/admin/p/<slug>`) and prints the matching `crap.pages.register(...)` snippet for `init.lua`. See [Custom Pages](../admin-ui/scenarios/05-custom-page.md).

#### `make route`

```bash
crap-cms make route [NAME] [-m <METHOD>] [-p <PATH>] [-f]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--method` | `-m` | `GET` | HTTP method |
| `--path` | `-p` | `/<name>` | URL path to mount at |
| `--force` | `-f` | — | Overwrite existing file |

Writes `routes/<name>.lua` (a typed `function(ctx)` handler) and prints the `crap.routes.register(...)` snippet for `init.lua`. See [`crap.routes`](../lua-api/routes.md).

#### `make slot`

```bash
crap-cms make slot [SLOT] [--file <NAME>] [--force]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--file` | `-f` | `widget` | Filename inside the slot directory |
| `--force` | — | — | Overwrite existing file (long form only — `-f` is taken by `--file`) |

Writes `templates/slots/<slot>/<file>.hbs`. See [Slots](../admin-ui/guides/slots.md) for the slot names.

#### `make node`

```bash
crap-cms make node [NAME] [-i] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--inline` | `-i` | Inline node (default: block-level) |
| `--force` | `-f` | Overwrite existing file |

Writes `lua/richtext_nodes/<name>.lua`, a custom richtext node definition, and prints the one-line `require(...)` to add to `init.lua` (the command never rewrites `init.lua` itself).

#### `make field`

```bash
crap-cms make field [NAME] [-b <BASE_TYPE>] [-f]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--base-type` | `-b` | `number` | Base field type to wrap: `text`, `number`, `textarea`, `select`, `radio`, `checkbox`, `date`, `email`, `json`, `code` |
| `--force` | `-f` | — | Overwrite existing files |

Writes three wired-together files: `templates/fields/<name>.hbs` (render template), `plugins/<name>.lua` (Lua wrapper plugin) and `static/components/crap-<name>.js` (Web Component skeleton).

#### `make theme`

```bash
crap-cms make theme [NAME] [-f]
```

Writes `static/styles/themes/themes-<name>.css` (a starter overriding the CSS tokens). See [Themes](../admin-ui/guides/themes.md).

#### `make component`

```bash
crap-cms make component [TAG] [-f]
```

Writes `static/components/<tag>.js`, a custom Web Component skeleton (`TAG` must contain a hyphen, e.g. `my-widget`). See [Components](../admin-ui/reference/components.md).

### `blueprint` — Manage saved blueprints

#### `blueprint save`

```bash
crap-cms blueprint save <NAME> [-f]
```

Saves the current config directory as a reusable blueprint (excluding `data/`, `uploads/`, `types/`). A `.crap-blueprint.toml` manifest is written with the CMS version and timestamp.

#### `blueprint use`

```bash
crap-cms blueprint use <NAME> [DIR]
```

Creates a new project from a saved blueprint. If the blueprint was saved with a different CMS version, a warning is printed (but the operation proceeds).

#### `blueprint list`

```bash
crap-cms blueprint list
```

Lists saved blueprints with collection/global counts and the CMS version they were saved with.

#### `blueprint remove`

```bash
crap-cms blueprint remove <NAME>
```

### `db` — Database tools

#### `db console`

```bash
crap-cms db console
```

Opens an interactive shell on the project database: `sqlite3 <path>` on SQLite, `psql <database.url>` on PostgreSQL. The client binary must be on `PATH`.

#### `db cleanup`

```bash
crap-cms db cleanup [--confirm]
```

| Flag | Description |
|------|-------------|
| `--confirm` | Actually drop orphan columns (default: dry-run report only) |

Detects columns in collection tables that don't correspond to any field in the current Lua definitions. System columns (`_`-prefixed like `_password_hash`, `_locked`) are always kept. Plugin columns are safe because plugins run during schema loading — their fields are part of the live definitions.

```bash
# Dry run — show orphans without removing them
crap-cms db cleanup

# Actually drop orphan columns
crap-cms db cleanup --confirm
```

### `export` — Export collection data

```bash
crap-cms export [-c <COLLECTION>] [-o <FILE>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Export only this collection (default: all) |
| `--output` | `-o` | Output file (default: stdout) |

Export includes `crap_version` and `exported_at` metadata in the JSON envelope. On import, a version mismatch produces a warning (but does not abort).

```bash
crap-cms export
crap-cms export -c posts -o posts.json
```

### `import` — Import collection data

```bash
crap-cms import <FILE> [-c <COLLECTION>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Import only this collection (default: all in file) |

```bash
crap-cms import backup.json
crap-cms import backup.json -c posts
```

Import is a **raw restore**, not a write through the service layer: each document is upserted by `id` straight into its table (existing rows with the same id are overwritten), join tables are rebuilt, and `_ref_count` is kept consistent. Lifecycle hooks, field validation, access rules and live events do **not** run. Every collection in the file must exist in the current Lua definitions — unknown collections are rejected **before anything is written**. Each collection is then imported in its own transaction, so a failure inside collection N (a malformed document, a DB error) rolls back N but leaves collections 1…N-1 imported.

### `typegen` — Generate typed definitions

Three subcommands, one per artifact / audience:

```bash
crap-cms typegen lua    [-o <DIR>]
crap-cms typegen client -l <LANG[,LANG...]> [-o <DIR>]
crap-cms typegen proto  -m <MODULE_PATH>    [-o <DIR>]
```

| Subcommand | Writes | Audience |
|------------|--------|----------|
| `lua` | `types/crap.lua` (API surface) + `types/hooks.lua` (per-collection hook narrowings) | Server-side Lua hook / access / job authors |
| `client` | `types/client.<ext>` per requested language (`ts`, `go`, `py`, `rs`) | External gRPC / REST API consumers |
| `proto` | `types/proto.rs` | Rust gRPC servers wanting `From<proto::Document>` conversions |

| Flag | Short | Default | Subcommand | Description |
|------|-------|---------|------------|-------------|
| `--lang` | `-l` | — | `client` | Comma list of client languages (`ts`, `go`, `py`, `rs`). Required. |
| `--module` | `-m` | — | `proto` | Rust module path to the prost-generated proto types (e.g. `"crate::proto"`). Required. |
| `--output` | `-o` | `<config>/types/` | all | Output directory. |

```bash
crap-cms typegen lua                            # types/crap.lua + types/hooks.lua
crap-cms typegen client -l ts                   # types/client.ts
crap-cms typegen client -l ts,go,py -o ./shared # multiple langs, custom dir
crap-cms typegen proto -m "crate::proto"        # types/proto.rs
```

Running `crap-cms typegen` with no subcommand prints help. Under `admin.dev_mode = true`, the `serve` command auto-regenerates `crap.lua` + `hooks.lua` on startup so hook authors don't have to remember `typegen lua` after editing collections — production startups skip this.

### `proto` — Export proto file

```bash
crap-cms proto [-o <PATH>]
```

Writes `content.proto` to stdout or the given path. No config directory needed.

```bash
crap-cms proto
crap-cms proto -o ./proto/
```

### `migrate` — Run database migrations

```bash
crap-cms migrate <create|up|down|list|fresh>
```

| Subcommand | Description |
|------------|-------------|
| `create <NAME>` | Generate a new migration file (e.g., `backfill_slugs`) |
| `up` | Sync schema + run pending migrations |
| `down [-s\|--steps N]` | Roll back last N migrations (default: 1) |
| `list` | Show all migration files with status |
| `fresh [-y\|--confirm]` | Drop all tables and recreate (destructive, requires confirmation) |

```bash
crap-cms migrate create backfill_slugs
crap-cms migrate up
crap-cms migrate list
crap-cms migrate down -s 2
crap-cms migrate fresh -y
```

### `backup` — Backup database

```bash
crap-cms backup [-o <DIR>] [-i]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output directory (default: `<config>/backups/`) |
| `--include-uploads` | `-i` | Also compress the uploads directory |

```bash
crap-cms backup
crap-cms backup -o /tmp/backups -i
```

### `restore` — Restore from backup

```bash
crap-cms restore <BACKUP> [-i] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--include-uploads` | `-i` | Also restore uploads from `uploads.tar.gz` if present |
| `--confirm` | `-y` | Required — confirms the destructive operation |

Replaces the current database with a backup snapshot. Cleans up stale WAL/SHM files.

```bash
crap-cms restore ./backups/backup-2026-03-07T10-00-00 -y
crap-cms restore /tmp/backups/backup-2026-03-07T10-00-00 -i -y
```

### `templates` — Manage admin template / static customizations

Extract the compiled-in admin templates and static files into your config directory for customization, then track drift between your customizations and upstream.

Each extracted file gets a `crap-cms:source <version>` header (in the file's native comment syntax) so `templates status` can report which version your customizations were extracted from.

#### `templates list`

```bash
crap-cms templates list [-t <TYPE>] [-v]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--type` | `-t` | Filter: `templates` or `static` (default: both) |
| `--verbose` | `-v` | Show full file tree with individual sizes (default: compact summary) |

```bash
crap-cms templates list
crap-cms templates list -t templates
crap-cms templates list -v
```

#### `templates extract`

```bash
crap-cms templates extract [PATHS...] [-a] [-t <TYPE>] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--all` | `-a` | Extract all files |
| `--type` | `-t` | Filter: `templates` or `static` (only with `--all`) |
| `--force` | `-f` | Overwrite existing files |

```bash
# Extract specific files
crap-cms templates extract layout/base.hbs styles.css

# Extract all templates
crap-cms templates extract --all --type templates

# Extract everything, overwriting existing
crap-cms templates extract --all --force
```

#### `templates status`

```bash
crap-cms templates status
```

Reports the relationship between every customized file in `<config_dir>/{templates,static}/` and the upstream embedded default. Each file is classified as one of:

- `✓ current` — extracted from the running version
- `⚠ behind: extracted from <ver>` — older version, may be missing upstream fixes
- `↑ ahead: extracted from <ver>` — newer than running (downgrade scenario)
- `= pristine (matches upstream)` — extracted but never customized
- `? no source header` — hand-written, or header was stripped
- `? unparseable source header` — header found but version isn't valid semver
- `✗ orphaned` — file no longer exists in the embedded upstream

#### `templates diff`

```bash
crap-cms templates diff <PATH>
```

Shows a unified diff between a customized file and its embedded default. The path is relative to the config dir (e.g. `templates/layout/base.hbs`, `static/styles.css`).

```bash
crap-cms templates diff templates/layout/base.hbs
```

### `fmt` — Format Handlebars templates

Format `.hbs` files in place using the project's built-in Handlebars formatter. Same role as `cargo fmt` for Rust or `biome check --write` for JS/CSS — keeps the templates' style consistent.

```bash
crap-cms fmt [PATHS...] [--check] [--stdio]
```

| Flag | Description |
|------|-------------|
| (none) | Format every `.hbs` under the given paths in place. Default scope is `templates/`. |
| `--check` | Don't write — exit non-zero if any file would change. CI gate. |
| `--stdio` | Read from stdin, write the formatted result to stdout. Used by editor formatter integrations. Mutually exclusive with `--check`. |

```bash
crap-cms fmt                              # format all templates/
crap-cms fmt templates/auth/              # one subtree
crap-cms fmt templates/fields/text.hbs    # one file
crap-cms fmt --check                      # CI: exit 1 if any file would change
cat my.hbs | crap-cms fmt --stdio         # editor pipe
```

The formatter is idempotent (`fmt(fmt(x)) == fmt(x)`) and applies the rule set documented in the [Admin UI: Template Formatter](../admin-ui/guides/template-formatter.md) page (block-helper indentation, attribute stacking, comment preservation, etc.).

**Editor integration (Neovim + conform.nvim):**

```lua
-- ~/.config/nvim/lua/plugins/conform.lua
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
```

**Pre-commit hook entry:**

```bash
echo "Running crap-cms fmt..."
cargo run --quiet --bin crap-cms -- fmt --check
```

### `jobs` — Manage background jobs

#### `jobs list`

```bash
crap-cms jobs list
```

Lists all defined jobs with their configuration (handler, schedule, queue, retries, timeout, concurrency).

#### `jobs trigger`

```bash
crap-cms jobs trigger <SLUG> [-d <DATA>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--data` | `-d` | `"{}"` | JSON data to pass to the job |

Manually queue a job for execution. Works even while the server is running (SQLite WAL allows concurrent access). Prints the queued job run ID.

#### `jobs status`

```bash
crap-cms jobs status [--id <ID>] [-s <SLUG>] [-l <LIMIT>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--id` | — | — | Show details for a specific run |
| `--slug` | `-s` | — | Filter by job slug |
| `--limit` | `-l` | `20` | Max results to show |

Show recent job runs. If `--id` is given, shows details for that specific run. Otherwise lists recent runs across all jobs.

#### `jobs cancel`

```bash
crap-cms jobs cancel [--slug <SLUG>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--slug`, `-s` | *(all)* | Only cancel pending jobs with this slug. Without it, cancels all pending jobs. |

Deletes pending jobs from the queue. Useful for clearing stuck or unwanted jobs that keep retrying.

#### `jobs purge`

```bash
crap-cms jobs purge [--older-than <DURATION>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--older-than` | `7d` | Delete completed/failed/stale runs older than this. Supports `Nd`, `Nh`, `Nm` formats. |

#### `jobs healthcheck`

```bash
crap-cms jobs healthcheck
```

Checks job system health and prints a summary: defined jobs, stale jobs (running but heartbeat expired), failed jobs in the last 24 hours, pending jobs waiting longer than 5 minutes, and scheduled jobs that have never completed a run.

Status: `healthy` (no issues), `warning` (failed jobs in the last 24h, jobs pending longer than 5 minutes, or scheduled jobs that have never completed a run), `unhealthy` (stale jobs detected). The **exit code** follows the status so it can gate CI/monitoring: `0` healthy, `2` warning, `1` unhealthy (mirrors `status --check`).

```bash
crap-cms jobs list
crap-cms jobs trigger cleanup_expired
crap-cms jobs status
crap-cms jobs status --id abc123
crap-cms jobs cancel
crap-cms jobs cancel -s process_inquiry
crap-cms jobs purge --older-than 30d
crap-cms jobs healthcheck
```

### `images` — Manage image processing queue

Inspect and manage the background image format conversion queue. See [Image Processing](../uploads/image-processing.md) for how to enable queued conversion.

#### `images list`

```bash
crap-cms images list [-s <STATUS>] [-l <LIMIT>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--status` | `-s` | — | Filter by status: `pending`, `processing`, `completed`, `failed` |
| `--limit` | `-l` | `20` | Max entries to show |

#### `images stats`

```bash
crap-cms images stats
```

Shows counts by status (pending, processing, completed, failed) and total.

#### `images retry`

```bash
crap-cms images retry [--id <ID>] [--all] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--id` | — | Retry a specific failed entry by ID |
| `--all` | — | Retry all failed entries |
| `--confirm` | `-y` | Required with `--all` |

#### `images purge`

```bash
crap-cms images purge [--older-than <DURATION>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--older-than` | `7d` | Delete completed/failed entries older than this. Supports `Nd`, `Nh`, `Nm`, `Ns` formats. |

```bash
crap-cms images list
crap-cms images list -s failed
crap-cms images stats
crap-cms images retry --id abc123
crap-cms images retry --all -y
crap-cms images purge --older-than 30d
```

### `trash` — Manage soft-deleted documents

Inspect, restore, and purge documents in the trash (only for collections with `soft_delete = true`). See [Soft Deletes](../collections/soft-deletes.md) for details on the soft-delete model.

#### `trash list`

```bash
crap-cms trash list [-c <COLLECTION>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Filter by collection slug (default: all collections with soft delete) |

#### `trash restore`

```bash
crap-cms trash restore <COLLECTION> <ID>
```

Restore a single trashed document back to the active list. Both `COLLECTION` and `ID` are positional arguments.

#### `trash purge`

```bash
crap-cms trash purge [-c <COLLECTION>] [--older-than <DURATION>] [--dry-run]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | — | Filter by collection slug (default: all soft-delete collections) |
| `--older-than` | — | `all` | Purge documents deleted more than this ago (e.g. `30d`, `24h`, `30m`), or `all` for every trashed document |
| `--dry-run` | — | — | Print what would be deleted without actually deleting |

#### `trash empty`

```bash
crap-cms trash empty <COLLECTION> [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--confirm` | `-y` | Required — confirms the destructive operation |

Permanently delete every trashed document in the given collection.

```bash
crap-cms trash list
crap-cms trash list -c posts
crap-cms trash restore posts abc123
crap-cms trash purge --older-than 7d
crap-cms trash purge -c posts --dry-run
crap-cms trash empty posts -y
```

### `mcp` — Start the MCP server (stdio)

Start an MCP (Model Context Protocol) server over stdio for AI assistant integration.

```bash
crap-cms mcp
```

Reads JSON-RPC 2.0 from stdin, writes responses to stdout. Use with Claude Desktop,
Cursor, VS Code, or any MCP-compatible client. See [MCP Overview](../mcp/overview.md)
for configuration and usage.

### `logs` — View and manage log files

```bash
crap-cms logs [-f] [-n <lines>]
crap-cms logs clear
```

View log output from file-based logging. Requires `[logging] file = true` in `crap.toml` (auto-enabled when running with `--detach`).

| Flag | Description |
|------|-------------|
| `-f`, `--follow` | Follow log output in real time (like `tail -f`) |
| `-n`, `--lines <N>` | Number of lines to show (default: 100) |

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `clear` | Remove old rotated log files, keeping only the current one |

```bash
crap-cms logs                # show last 100 lines
crap-cms logs -f             # follow in real time
crap-cms logs -n 50          # show last 50 lines
crap-cms logs clear          # remove old rotated files
```

Log files are stored in `data/logs/` (or the path configured in `[logging] path`). Old files are automatically pruned on startup based on `max_files`. See [Configuration Reference](../configuration/crap-toml.md) for all logging options.

### `update` — Manage installed versions

```bash
crap-cms update [-y] [--force]
crap-cms update <SUBCOMMAND>
```

Without a subcommand, checks for a newer release and installs + activates it (with confirmation prompt).

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip confirmation prompts |
| `--force` | Allow self-update even when the binary looks distro-managed |

#### `update check`

```bash
crap-cms update check
```

Compare current version to the latest GitHub release. Exit code 0 if up-to-date, 1 if newer is available.

#### `update list`

```bash
crap-cms update list
```

List available release tags, marking installed versions and the active one.

#### `update install`

```bash
crap-cms update install <VERSION> [--reinstall]
```

Download, verify (SHA256), and stage a version in the local store (`~/.local/share/crap-cms/versions/`). Does not activate — use `update use` to switch.

#### `update use`

```bash
crap-cms update use <VERSION>
```

Switch the `current` symlink to the given installed version. Also auto-installs shell completions for the user's login shell (bash, zsh, or fish) — see `update completions` for where files are written and how the zsh `$fpath` is probed.

#### `update uninstall`

```bash
crap-cms update uninstall <VERSION>
```

Remove an installed version from the store. Refuses to uninstall the active version. If this removes the last installed version, auto-installed shell completion files are cleaned up too.

#### `update where`

```bash
crap-cms update where
```

Print the resolved path of the currently active binary.

#### `update completions`

```bash
crap-cms update completions <SHELL>
crap-cms update completions <SHELL> --uninstall
crap-cms update completions --uninstall
```

Generate shell completions (to stdout) or remove installed files. Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

For bash, zsh, and fish, completions are also auto-installed after `update use` and bare `update`:

- **Zsh**: the install directory is chosen by probing `$fpath` (`zsh -i -c 'print -l $fpath'`). If `~/.zfunc` is already on `$fpath`, the file goes there; otherwise the first user-owned directory on `$fpath` is used. If neither is available, the file is written to `~/.zfunc` and an activation hint (`fpath=(~/.zfunc $fpath)` before `compinit`) is shown on every install until it's wired up.
- **Bash**: installed under `$XDG_DATA_HOME/bash-completion/completions/crap-cms`. A hint is emitted if the `bash-completion` entry point isn't present on the system.
- **Fish**: installed under `$XDG_CONFIG_HOME/fish/completions/crap-cms.fish` — auto-loaded by fish.

`--uninstall` without a shell removes every auto-installed completion file. With a shell, it removes just that shell's file. `update uninstall` of the last installed version also runs this cleanup automatically.

```bash
crap-cms update                          # install latest + activate
crap-cms update -y                       # non-interactive
crap-cms update check                    # check for updates
crap-cms update list                     # list available versions
crap-cms update install v0.1.0-alpha.7   # download + verify
crap-cms update use v0.1.0-alpha.7       # switch to version
crap-cms update uninstall v0.1.0-alpha.6 # remove old version
crap-cms update where                    # print active binary path
crap-cms update completions bash         # print bash completions
eval "$(crap-cms update completions bash)"  # source directly
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `CRAP_CONFIG_DIR` | Path to the config directory (same as `--config` flag; flag takes priority) |
| `RUST_LOG` | Controls log verbosity. Defaults: `crap_cms=info` for `serve`, `work` and `mcp`; `crap_cms=debug,info` for `serve`/`work` when `[admin] dev_mode = true`; `crap_cms=error` for every other command. Example: `RUST_LOG=crap_cms=trace` |
| `CRAP_LOG_FORMAT` | Set to `json` for structured JSON log output (same as `--json` flag) |
| `CRAP_NO_UNICODE` | Force ASCII glyphs (`+ ! x >`) instead of Unicode (`✓ ⚠ ✗ →`) in CLI output. Truthy values (`1`, `true`, `yes`, `on`) enable it. |
| `CRAP_FORCE_UNICODE` | Force Unicode glyphs even when terminal detection says otherwise. Truthy values (`1`, `true`, `yes`, `on`) enable it. |

> **Reserved:** `_CRAP_DETACHED` is set internally by `serve`/`work` on the
> re-exec'd background child so it can auto-enable file logging. It is not a
> user-facing knob — do not set it yourself.
