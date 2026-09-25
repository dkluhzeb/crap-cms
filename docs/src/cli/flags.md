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

| `--stop` | Stop a running detached instance (SIGTERM; running jobs are drained up to the longest configured `[jobs.queues.<name>] timeout` plus five minutes, then SIGKILL; a Lua job's own `timeout` is not part of that deadline — raise the matching queue timeout for long Lua jobs) |
| `--restart` | Restart a running detached instance (stop + start) |
| `--status` | Show whether a detached instance is running (PID, uptime) |
| `--json` | Output logs as structured JSON (for log aggregation; forwarded to the detached child) |
| `--only <admin\|grpc>` | Start only the specified server. Omit to start both. `api` is accepted as a backward-compatible alias for `grpc`. |
| `--no-scheduler` | Disable the background job scheduler |

`--detach`, `--stop`, `--restart`, and `--status` are mutually exclusive.
A start — foreground or `--detach` — refuses when `data/crap.pid` names a live process; the PID file is written only after startup succeeded and removed on every exit path, so a failed start never hides the running server from `--stop`/`--status`.

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
| `--queues <list>` | Comma-separated queue names to process (default: all). Enforced in the claim itself — the worker never claims a run outside its queues; a name no job uses is warned about at start |
| `--concurrency <n>` | Override `jobs.max_concurrent` for this worker |
| `--no-cron` | Skip cron scheduling (let another worker handle it). The retention purges still run on this worker — they are single-winner housekeeping, not cron jobs |

As for `serve`: a start refuses when `data/crap-worker.pid` names a live worker, and the file is written only once the worker is up.

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

Runs a best-practice audit with 29 checks across four categories:

**Security:**
- Auth secret shorter than 32 characters
- Auth secret that looks like a placeholder
- Brute-force protection disabled (`max_login_attempts = 0`)
- `default_deny = false` (collections publicly accessible)
- Collections without access rules
- A collection whose draft or trash view is publicly readable (drafts or soft delete enabled, no `access.draft` / `access.trash` or `access.update` rule, `default_deny = false`) — even when `read` is set
- A global whose draft view is publicly readable (drafts enabled, no `access.draft` or `access.update` rule, `default_deny = false`)
- Auth collection with `password_login` but no `bearer` method (login issues a token nothing accepts)
- More than one always-active auth strategy on the same surface
- More than one auth strategy bound to the same header on the same surface
- gRPC rate limiting disabled with auth collections
- CORS wildcard origin with credentials

**Performance:**
- `max_depth > 3` (N+1 query growth)
- Cache disabled with relationship fields
- Pool size too small
- Connection timeout too aggressive
- Response compression disabled
- `pagination.max_limit > 500`
- More than 10 hooks on a collection
- More than 3 before_change hooks on a collection
- More than 5 collections with `live_mode = "full"`

**Configuration:**
- `dev_mode` enabled
- `default_depth` exceeds `max_depth`
- Email provider set to `"log"` with `verify_email` enabled

**Operations:**
- Pending migrations
- Auth collection without soft_delete
- Upload collection without versioning
- Soft delete without retention policy
- Auth collection with no users

A check that can't read what it needs — the migration status, or an auth
collection's user count — reports that as its own warning instead of
passing or guessing.

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

Every subcommand that acts on one user finds it with the same flags — the
user is chosen interactively when neither `--email` nor `--id` is given:

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | `users` | Auth collection slug (every `user` subcommand) |
| `--email` | `-e` | — | Find the user by email |
| `--id` | — | — | Find the user by ID |

#### `user create`

```bash
crap-cms user create [-c <COLLECTION>] [-e <EMAIL>] [-p <PASSWORD> | --password-stdin] [-f <KEY=VALUE>]...
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | `users` | Auth collection slug |
| `--email` | `-e` | — | User email (prompted if omitted) |
| `--password` | `-p` | — | User password (prompted if omitted). Visible in the process list and shell history |
| `--password-stdin` | — | — | Read the password from the first line of standard input |
| `--field` | `-f` | — | Extra fields as key=value (repeatable). Array, blocks and group fields take JSON; list fields take a JSON array (see [CLI User Creation](../authentication/cli-user-creation.md#field-handling)) |

```bash
# Interactive (prompts for password)
crap-cms user create -e admin@example.com

# Non-interactive
printf '%s\n' "$ADMIN_PASSWORD" | crap-cms user create \
    -e admin@example.com \
    --password-stdin \
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

Deletes through the same service as the admin UI and delete hooks run. A user of a soft-delete collection is moved to the trash; `trash purge` later refuses it while other documents still reference it. On other collections, a user other documents still reference is refused right away. With Redis live updates, the user's open live streams on `serve` are closed.

The delete clears the configured populate cache. With `[cache] backend = "redis"` that is the cache `serve` reads, so it drops what the delete made stale. With the default in-process `memory` backend, the cache cleared is the CLI's own: a running `serve` keeps its entries until its own next write clears them, or its periodic clear when `[cache] max_age_secs` is set. The same holds for every CLI write below.

#### `user lock` / `user unlock`

```bash
crap-cms user lock [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
crap-cms user unlock [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

Locking goes through the same service op as the admin and gRPC: it bumps the user's session version, so every token and cookie issued before it is rejected, and tears down the user's open live streams. With Redis live updates this reaches `serve`'s subscribers. With in-process transports (the default) no other process hears the signal: `serve` still rejects the revoked session on the user's next request, but a stream already open stays open until it reconnects. A configured Redis that can't be reached fails the command before it writes.

#### `user reset-totp`

```bash
crap-cms user reset-totp [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--confirm` | `-y` | Skip confirmation prompt |

Clears a user's TOTP enrollment (secret, confirmation, replay guard); they
re-enroll on their next login. Requires `mfa = "totp"` on the collection;
prompts for confirmation unless `-y` is passed. Like `user change-password`,
it ends the user's sessions and live streams the way `user lock` does.

#### `user verify` / `user unverify`

```bash
crap-cms user verify [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
crap-cms user unverify [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

Manually mark a user's email as verified or unverified. Only works on collections with `verify_email = true`. Useful when email is not configured. `unverify` ends the user's sessions and live streams the way `user lock` does.

#### `user change-password`

Change a user's password. Prompts for the new password unless `-p` or
`--password-stdin` is given.

```bash
crap-cms user change-password [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-p <PASSWORD> | --password-stdin]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--password` | `-p` | New password. Visible in the process list and shell history |
| `--password-stdin` | — | Read the new password from the first line of standard input |

The new password must pass `[auth.password_policy]`. The change ends every session opened with the old password, clears any pending reset link, and tears down the user's live streams, as `user lock` does.

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
| `--field` | `-F` | Target field name (field hooks only — `*` scaffolds an any-field hook with the single-argument `field_hook(fn)` form; watched field for condition hooks) |
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
- **`after_read` hooks:** `crap.read_hook.Posts`, `crap.read_hook.global_site_settings`
  (wrapped in `read_hook(fn)`; `ctx.data` is the read document)
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
| `--schedule` | `-s` | — | Cron expression (e.g., `"0 3 * * *"`); day-of-week is crontab-numbered (`0`/`7` Sunday … `6` Saturday) |
| `--queue` | `-q` | `default` | Queue name |
| `--retries` | `-r` | *(queue default)* | Max retry attempts. Omit to let the job inherit `[jobs.queues.<queue>] retries` at runtime; pass an explicit value (including `0`) to write a fixed `retries` into the generated Lua |
| `--timeout` | `-t` | 60 | Timeout in seconds (at least 1) |
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

Writes three wired-together files: `templates/fields/<name>.hbs` (render template), `plugins/<name>.lua` (Lua wrapper plugin) and `static/components/crap-<name>.js` (Web Component skeleton). All three targets are checked before the first is written: an existing file (without `--force`) or a name that makes no valid component tag (`crap-<name>` may hold only lowercase letters, digits and `-`, so no `_`) refuses the whole scaffold and leaves nothing behind.

#### `make theme`

```bash
crap-cms make theme [NAME] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--force` | `-f` | Overwrite existing file |

Writes `static/styles/themes/themes-<name>.css` (a starter overriding the CSS tokens). See [Themes](../admin-ui/guides/themes.md).

#### `make component`

```bash
crap-cms make component [TAG] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--force` | `-f` | Overwrite existing file |

Writes `static/components/<tag>.js`, a custom Web Component skeleton (`TAG` must contain a hyphen, e.g. `my-widget`). See [Components](../admin-ui/reference/components.md).

### `blueprint` — Manage saved blueprints

#### `blueprint save`

```bash
crap-cms blueprint save <NAME> [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--force` | `-f` | Overwrite an existing blueprint of that name |

Saves the current config directory as a reusable blueprint. A `.crap-blueprint.toml` manifest is written with the CMS version and timestamp. `crap.toml` must load (it is read to find the configured database and log paths).

A blueprint is meant to be shared, so the project's state and secrets stay out of it:

- `data/`, `uploads/`, `types/` (regenerated on `use`) and `backups/` at the top level;
- the configured database file and its `-wal` / `-shm` / `-journal` sidecars, and the configured log directory, wherever `crap.toml` puts them;
- anywhere in the tree: any SQLite database file and its sidecars, any backup directory (`manifest.json` beside `crap.db`, e.g. a `backup -o` inside the project), generated auth-secret files (`.jwt_secret*`) and in-flight upload staging files (`*.crap-tmp`).

`crap.toml` itself is copied verbatim — keep credentials in it as `${ENV_VAR}` references if the blueprint will be shared. Symlinks are neither followed nor copied (each skipped one is reported). The copy is built under a hidden name and swapped into place only when complete, so a failed save — including `--force` over an existing blueprint — leaves the previous blueprint intact.

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
crap-cms db console [--skip-config-validation]
```

Opens an interactive shell on the project database: `sqlite3 <path>` on SQLite, `psql <database.url>` on PostgreSQL. The client binary must be on `PATH`.

| Flag | Description |
|------|-------------|
| `--skip-config-validation` | Run even if `crap.toml` fails validation (see below) |

Every command refuses a `crap.toml` that fails validation. `--skip-config-validation` is the recovery escape hatch for when an upgrade made an existing config invalid: the command prints the validation error as a warning and runs on the config as loaded, so you can back up and inspect the database before fixing the config. Only `backup`, `restore`, `db console` and the `logs` tail accept it.

#### `db cleanup`

```bash
crap-cms db cleanup [-y] [--drop-tables]
```

| Flag | Description |
|------|-------------|
| `--confirm`, `-y` | Apply the changes: drop orphan columns (of collection, global and junction tables) and delete stale-locale junction rows (default: dry-run report only) |
| `--drop-tables` | Together with `-y`: also drop orphan tables — a collection, global, versions or junction table whose definition no longer exists. Without it they are only reported; the boot warns about them too |

Detects columns in collection and global tables that don't correspond to any field in the current Lua definitions, and rows in array, blocks and relationship junction tables whose `_locale` is no longer configured. System columns (`_`-prefixed like `_password_hash`, `_locked`) are always kept. Plugin columns are safe because plugins run during schema loading — their fields are part of the live definitions.

With `--confirm`, everything is applied in one transaction: a failure leaves the database as it was. The rows, columns and tables removed can hold relationship and upload references, so the same transaction recomputes every document's reference count (the count behind [delete protection](../relationships/delete-protection.md)); a reference that only a deleted row held no longer blocks deleting its target. What was dropped and deleted is printed only after the transaction commits, so a cleanup that fails reports nothing as done.

```bash
# Dry run — show orphans without removing them
crap-cms db cleanup

# Actually drop orphan columns
crap-cms db cleanup --confirm
```

### `export` — Export collection data

```bash
crap-cms export [-c <COLLECTION>] [-o <FILE>] [--include-credentials]
```

The file is written to `<FILE>.tmp` beside the target and renamed into place once complete, so an interrupted export never leaves a truncated file under the final name.

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Export only this collection (default: all) |
| `--output` | `-o` | Output file (default: stdout) |
| `--include-credentials` | | Also export each account's password hash, lock, session version, verification and TOTP state |

Export includes `crap_version` and `exported_at` metadata in the JSON envelope. On import, a version mismatch produces a warning (but does not abort).

Every document is exported, trashed ones included (with their `_deleted_at`), along with timezone companions (`<field>_tz`) and the ids of array and blocks rows. A localized array, blocks or has-many field carries every locale's rows as `{ "<locale>": rows }`, like a localized column.

Without `--include-credentials` an export carries no credentials, and importing an auth collection from it leaves new accounts without a password (an account that already exists keeps its stored one). With the flag, each account carries a `_credentials` object. One-time tokens (password reset, email verification, MFA codes) are never exported. Treat a file exported with the flag like a database dump.

Export is an **operator tool that reads the database directly**: it dumps every document of every selected collection — published, draft and trashed alike — and neither collection nor field `access` rules nor read hooks run (the same trust model `import` states for the write direction). Treat an export file like a database dump.

Export covers **collections only** — globals are not part of the envelope. For a complete copy of a deployment (globals, versions, uploads) use `backup` / `restore`.

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

Import is a **raw restore**, not a write through the service layer: each document is upserted by `id` straight into its table (existing rows with the same id are overwritten), join tables are rebuilt with their exported row ids, and `_ref_count` is kept consistent — counts are settled once every document is written, so a document may reference one later in the file. Trashed documents import trashed. Email and text values are stored in canonical form, as every write stores them. Lifecycle hooks, field validation, access rules and live events do **not** run.

Accounts that end the import without a password can't log in until one is set; import warns when that happens. Credentials imported over an existing account revoke its sessions. An account whose TOTP secret was sealed with a different auth secret is refused, naming it: import into an installation with the same auth secret, or export without `--include-credentials`.

Every collection in the file must exist in the current Lua definitions, and the whole import runs in **one transaction**: an unknown collection, a malformed document or a DB error leaves nothing imported.

After the commit, the import clears the configured populate cache (see `user delete` for what that reaches) and tears down the live streams of every account it overwrote, since their roles, lock state or credentials may have changed. With Redis live updates this reaches `serve`'s subscribers. A configured Redis that can't be reached fails the import before anything is written.

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

`typegen lua`, like `typegen client`, fails without writing its types files when two collections, globals or fields would generate the same type name (collections `a1` and `a_1` are both `A1`); rename one. Slugs and field names that aren't Lua identifiers are declared as quoted keys (`crap.collections["2fa"]`, `---@field ["2fa"]? string`) — see [non-identifier names](../lua-api/collections.md#slugs-and-field-names-that-arent-lua-identifiers).

Running `crap-cms typegen` with no subcommand prints help. Under `admin.dev_mode = true`, the `serve` command auto-regenerates `crap.lua` + `hooks.lua` on startup so hook authors don't have to remember `typegen lua` after editing collections — production startups skip this.

### `proto` — Export proto file

```bash
crap-cms proto [-o <PATH>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output path (file or directory). Omit to write to stdout |

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

Each migration runs in its own transaction together with its record in
`_crap_migrations`. Two runs racing each other cannot apply or roll back the
same migration twice: the second `up` fails on the already-recorded file, and
the second `down` finds the record already gone and is rolled back.

`fresh` drops every table and recreates the schema in **one transaction**
under the schema-sync lock, so a failure leaves the database as it was. It
holds the exclusive instance lock of the local project only: on a PostgreSQL
database shared by several nodes, stop every node's `serve` / `work` first —
their running servers see an empty database the moment `fresh` commits.

`fresh` refuses while a `serve`, `work` or stdio `mcp` process uses the project, and keeps them from starting until it finishes.

Each migration runs in its own transaction, together with the bookkeeping row that marks it applied (or removes it on `down`). Its Lua CRUD writes behave like writes made on the server: after the commit the configured cache is cleared, live events reach `serve`'s subscribers (over Redis when `[live]` uses it), and the files of upload documents the migration hard-deleted are removed. A migration that fails rolls back with every file still in storage. The command therefore builds the same infrastructure as `user delete`, and a configured Redis that can't be reached fails it before any migration runs — for `fresh`, before any table is dropped.

### `backup` — Backup database

```bash
crap-cms backup [-o <DIR>] [-i] [--skip-config-validation]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output directory (default: `<config>/backups/`) |
| `--include-uploads` | `-i` | Also compress the uploads directory (the command fails if `tar` is missing or fails) |
| `--skip-config-validation` | | Run even if `crap.toml` fails validation — back up before fixing a config an upgrade invalidated (see [`db console`](#db-console)) |

```bash
crap-cms backup
crap-cms backup -o /tmp/backups -i
```

`backup` copies the SQLite database file; back up a Postgres database with `pg_dump`. `--include-uploads` archives the local `uploads/` directory; uploads kept in S3 or custom storage need that service's own backup.

`backup` runs beside a live `serve`. With `--include-uploads`, the uploads tree is captured into a private staging directory under `data/` as hard links (copies where the filesystem refuses links) once before the database snapshot and once after it, and the archive is built from that capture, never from the live tree. Every file present when the snapshot was taken is therefore in the archive even if it is deleted or replaced meanwhile, files uploaded during the snapshot are added, and in-flight `*.crap-tmp` staging files are left out — a busy site no longer makes `tar` fail with "file changed as we read it". The only file that can be missing is one uploaded and removed again within the seconds the database snapshot takes. Hard links cost no space; where they are refused (for example when `uploads/` is a mount on another filesystem than `data/`) the capture copies every file, so the backup then needs free space for a second copy of the uploads while it runs. A backup that is killed before it finishes leaves its capture behind; the next `backup --include-uploads` removes it.

Everything the backup writes is owner-only — the backup directory `0700`, `crap.db`, `uploads.tar.gz` and `manifest.json` `0600` — whatever the umask or the permissions of the `--output` directory (on Unix; on Windows the files inherit the directory's ACL, so choose a private `--output`): the snapshot holds password and API-key hashes and sealed TOTP secrets. When the auth secret is generated (`[auth] secret` is empty), the backup also contains it as `jwt_secret` (also `0600`) — sessions, TOTP enrollments and `crap.crypto` ciphertext in the database depend on it. Keep backups as private as the secret.

### `restore` — Restore from backup

```bash
crap-cms restore <BACKUP> [-i] [-y] [--skip-config-validation]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--include-uploads` | `-i` | Also restore uploads from `uploads.tar.gz` if present (skipped with a note when `[upload] storage` is not `local`, as with `backup`) |
| `--confirm` | `-y` | Required — confirms the destructive operation |
| `--skip-config-validation` | | Run even if `crap.toml` fails validation (see [`db console`](#db-console)) |

Replaces the current database with a backup snapshot. Cleans up stale WAL/SHM files. Refuses while a `serve`, `work` or stdio `mcp` process or any other CLI command uses the project (they hold `data/crap.lock`), and keeps them from starting until the restore finishes. A backed-up auth secret is written back to `data/.jwt_secret`; a different secret already there is kept as `data/.jwt_secret.pre-restore-<timestamp>`, so repeated restores never overwrite an earlier one — unless the restore's own config load generated it, when it holds nothing worth keeping. When `crap.toml` sets `[auth] secret`, that secret takes precedence and the restore warns that the backup's secret isn't used. The restored database gets the permissions of the database it replaces, not the backup's owner-only mode.

A backup taken with an older crap-cms is restored and schema-migrated on the next start. A backup taken with a **newer** crap-cms is refused before anything is touched: restoring it would be a downgrade — the database carries schema and one-time migration state the older binary does not know. Restore it with that version or later (`crap-cms update use <version>`).

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
- `· user-original (no upstream counterpart)` — your own file (a custom page, slot widget, component or theme) with no built-in default to drift from; informational, never a warning

#### `templates diff`

```bash
crap-cms templates diff <PATH>
```

Shows a unified diff between a customized file and its embedded default. The path is relative to the config dir (e.g. `templates/layout/base.hbs`, `static/styles.css`).

```bash
crap-cms templates diff templates/layout/base.hbs
```

#### `templates layout`

```bash
crap-cms templates layout
```

One-time migration assistant for config dirs customized before the current template/static layout. **Read-only** — it never moves or rewrites a file. It reports:

1. files on an old-layout path, as `OLD → NEW`;
2. a copy-pasteable recipe of `mkdir -p` and move commands — `git mv` / `git rm` when the config dir is in a git work tree, plain `mv` / `rm` otherwise;
3. what to verify after moving, which the tool can't rewrite safely (imports inside moved JS files, partial-by-path references in `.hbs` files, CSS `@import url(...)`);
4. files under the overlay roots that match neither layout — your own files, listed so you know none were lost.

On a config dir already on the current layout it says so and exits. See [Migrating from the old layout](../admin-ui/upgrade/migrating-from-old-layout.md).

### `fmt` — Format Handlebars templates

Format `.hbs` files in place using the project's built-in Handlebars formatter. Same role as `cargo fmt` for Rust or `biome check --write` for JS/CSS — keeps the templates' style consistent.

```bash
crap-cms fmt [PATHS...] [--check] [--stdio] [--follow-symlinks]
```

| Flag | Description |
|------|-------------|
| (none) | Format every `.hbs` under the given paths in place. Default scope is `templates/`. |
| `--check` | Don't write — exit non-zero if any file would change. CI gate. |
| `--stdio` | Read from stdin, write the formatted result to stdout. Used by editor formatter integrations. Mutually exclusive with `--check`. |
| `--follow-symlinks` | Follow symlinks. Off by default: a symlinked directory is not descended and a symlinked `.hbs` is skipped rather than written through to its target (which may live outside the tree); a symlink named directly as a path is refused. |

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
crap-cms jobs trigger <SLUG> [-d <DATA>] [-p <PRIORITY>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--data` | `-d` | `"{}"` | JSON data to pass to the job |
| `--priority` | `-p` | the job's `priority` (or `0`) | Scheduling priority — higher runs sooner. Negative values are accepted |

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
crap-cms jobs cancel [--slug <SLUG>] [--id <ID>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--slug`, `-s` | *(all)* | Only cancel pending jobs with this slug. Without it, cancels all pending jobs. |
| `--id` | — | Cancel exactly this pending run — the precise alternative to clearing a whole slug (e.g. one queued bulk operation). Takes precedence over `--slug`. A run that was already claimed is left alone and reported. |

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
crap-cms images retry [--id <ID>] [--all] [-y] [-p <PRIORITY>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--id` | — | — | Retry a specific failed entry by ID |
| `--all` | — | — | Retry all failed entries |
| `--confirm` | `-y` | — | Required with `--all` |
| `--priority` | `-p` | `0` | Scheduling priority for the retried job(s) — higher runs sooner, so an urgent retry can jump the queue. Negative values are accepted |

#### `images purge`

```bash
crap-cms images purge [--older-than <DURATION>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--older-than` | `7d` | Delete completed, failed and stale entries that finished longer ago than this (age counts from completion, or creation for a stale run without one). Supports `Nd`, `Nh`, `Nm`, `Ns` formats. |

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

Restores through the same service as the admin UI's undelete: the collection's `before_change` and `after_change` hooks run with `ctx.operation = "undelete"` (a `before_change` hook that errors leaves the document in the trash), the cache is cleared, and an undelete event is published. With `[live] transport = "redis"` the event reaches `serve`'s subscribers; a configured Redis that can't be reached fails the command before anything is restored. Collection access rules don't apply to the CLI.

#### `trash purge`

```bash
crap-cms trash purge [-c <COLLECTION>] [--older-than <DURATION>] [--dry-run] [-y]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | — | Filter by collection slug (default: all soft-delete collections) |
| `--older-than` | — | `all` | Purge documents deleted more than this ago (e.g. `30d`, `24h`, `30m`), or `all` for every trashed document |
| `--dry-run` | — | — | Print what would be deleted without actually deleting |
| `--confirm` | `-y` | — | Required unless `--dry-run` — confirms the destructive operation; without it the candidates are listed and nothing is deleted |

#### `trash empty`

```bash
crap-cms trash empty <COLLECTION> [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--confirm` | `-y` | Required — confirms the destructive operation |

Permanently delete every trashed document in the given collection.

`trash purge` and `trash empty` publish a delete event for each purged document once the purge has committed, gated by the `trash` view, like every other permanent delete (see [Live Updates](../live-updates/overview.md#access-control)). With `[live] transport = "redis"` the events reach `serve`'s subscribers; a configured Redis that can't be reached fails the command before anything is deleted. A preview (`--dry-run`, or no `--confirm`) publishes nothing.

Both run the same purge as the scheduled retention purge, and they are safe to run beside `serve`: each document is locked and re-checked inside the purge's transaction, so one restored (or trashed again more recently than `--older-than`) after the candidates were listed is left alone, as is one other documents still reference. Skipped documents are reported by reason.

```bash
crap-cms trash list
crap-cms trash list -c posts
crap-cms trash restore posts abc123
crap-cms trash purge --older-than 7d -y
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
| `--skip-config-validation` | Show the logs even if `crap.toml` fails validation (see [`db console`](#db-console)); not accepted by `clear`, which deletes files |

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `clear` | Remove old rotated log files, keeping only the current one |

`-f`, `-n` and `--skip-config-validation` shape the tail only — `logs -f clear` is refused rather than silently ignoring them.

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
| `-y`, `--yes` | Skip confirmation prompts. Only bare `update` (the "install and switch?" prompt) and `update use --force` (the prompt before a regular file on `$PATH` is replaced) ask anything — `install`, `uninstall`, `completions` and the read-only subcommands never prompt and ignore it |
| `--force` | Allow bare `update` and `update use` even when the running binary looks distro-managed (`/usr`, `/opt`, `/nix`, `/bin`, `/sbin`), **and** repoint the `crap-cms` on `$PATH` at the store after switching (see `update use`) |

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

Download, verify (SHA256), and stage a version in the local store (`~/.local/share/crap-cms/versions/`). Does not activate — use `update use` to switch. Staging touches only the store, never the running binary or the one on `$PATH`, so it works — without `--force` — even when the running binary is distro-managed.

`<VERSION>` must be a release tag (`v0.1.0-alpha.5` or `0.1.0-alpha.5`); anything that is not a semver version is refused by `install`, `use` and `uninstall` alike. Every published release is searched, not only the most recent page.

The download is written to a hidden partial file inside the version's store directory, fsynced, checked against the release's `SHA256SUMS`, and only then renamed over `versions/<VERSION>/crap-cms`. An interrupted or corrupt download leaves nothing behind that counts as installed, and `--reinstall` of the active version swaps the binary atomically (a running server keeps the file it started from). A stalled connection fails after two minutes without data; a slow but progressing download is never cut off.

`SHA256SUMS` is published in the same GitHub release as the binary, so the check proves the download is complete and uncorrupted — it is an integrity check, not a signature. Releases are not signed; if you need provenance, verify the tag and the release workflow on GitHub yourself.

#### `update use`

```bash
crap-cms update use <VERSION> [--force] [-y]
```

Switch the `current` symlink to the given installed version. Refused when the running binary looks distro-managed, unless `--force` is passed.

With `--force` it also **repoints the `crap-cms` on `$PATH` at the store**, so the shell runs the new version next time: a symlink there is replaced silently; a regular file (for example a `cargo install` build in `~/.local/bin`) is replaced with a symlink after a confirmation prompt, which `-y` skips; a distro-managed location is refused even with `--force`. Bare `update --force` does the same after installing the latest release.

`update use` also auto-installs shell completions for the user's login shell (bash, zsh, or fish) — see `update completions` for where files are written and how the zsh `$fpath` is probed.

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
