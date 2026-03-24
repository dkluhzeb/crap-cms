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
crap-cms serve [-d] [--json] [--only <admin|api>] [--no-scheduler]
```

| Flag | Description |
|------|-------------|
| `-d`, `--detach` | Run in the background (prints PID and exits) |
| `--json` | Output logs as structured JSON (for log aggregation) |
| `--only <admin\|api>` | Start only the specified server. Omit to start both. |
| `--no-scheduler` | Disable the background job scheduler |

```bash
crap-cms serve
crap-cms serve -d
crap-cms serve --json
crap-cms serve --only admin       # admin UI only
crap-cms serve --only api         # gRPC API only
crap-cms serve --no-scheduler     # both servers, no scheduler
crap-cms serve --only admin --no-scheduler
crap-cms serve -d --only api      # detached, API only
```

### `status` — Show project status

```bash
crap-cms status
```

Prints collections (with row counts), globals, DB size, and migration status.

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

Runs an interactive wizard that scaffolds a complete config directory. Defaults to `./crap-cms` if no directory is given.

The wizard prompts for:

| Prompt | Default | Description |
|--------|---------|-------------|
| Admin port | `3000` | Port for the admin UI |
| gRPC port | `50051` | Port for the gRPC API |
| Enable localization? | No | If yes, prompts for default locale and additional locales |
| Default locale | `en` | Default locale code (only if localization enabled) |
| Additional locales | — | Comma-separated (e.g., `de,fr`) |
| Create auth collection? | Yes | Creates a `users` collection with email/password login |
| Create first admin user? | Yes | Prompts for email and password immediately |
| Create upload collection? | Yes | Creates a `media` collection for file/image uploads |
| Create another collection? | No | Repeat to add more collections interactively |

A 64-character auth secret is auto-generated and written to `crap.toml`.

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
| `access` | `read`, `create`, `update`, `delete` |
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
| `--retries` | `-r` | 0 | Max retry attempts |
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

Opens an interactive `sqlite3` session on the project database.

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

### `typegen` — Generate typed definitions

```bash
crap-cms typegen [-l <LANG>] [-o <DIR>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--lang` | `-l` | `lua` | Output language: `lua`, `ts`, `go`, `py`, `rs`, `all` |
| `--output` | `-o` | `<config>/types/` | Output directory for generated files |

```bash
crap-cms typegen
crap-cms typegen -l all
crap-cms typegen -l ts -o ./client/src/types
```

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

### `templates` — List and extract default admin templates

Extract the compiled-in admin templates and static files into your config directory for customization.

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

Exit status: `healthy` (no issues), `warning` (failed or long-pending jobs), `unhealthy` (stale jobs detected).

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

### `mcp` — Start the MCP server (stdio)

Start an MCP (Model Context Protocol) server over stdio for AI assistant integration.

```bash
crap-cms mcp
```

Reads JSON-RPC 2.0 from stdin, writes responses to stdout. Use with Claude Desktop,
Cursor, VS Code, or any MCP-compatible client. See [MCP Overview](../mcp/overview.md)
for configuration and usage.

## Environment Variables

| Variable | Description |
|----------|-------------|
| `CRAP_CONFIG_DIR` | Path to the config directory (same as `--config` flag; flag takes priority) |
| `RUST_LOG` | Controls log verbosity. Default: `crap_cms=debug,info` for `serve`, `crap_cms=error` for all other commands. Example: `RUST_LOG=crap_cms=trace` |
| `CRAP_LOG_FORMAT` | Set to `json` for structured JSON log output (same as `--json` flag) |
