# Command-Line Reference

```
crap-cms <COMMAND> [OPTIONS]
```

Use `crap-cms --help` to list all commands, or `crap-cms <command> --help` for details on a specific command.

## Global Flags

| Flag | Description |
|------|-------------|
| `-V`, `--version` | Print version and exit |
| `-h`, `--help` | Print help |

## Commands

### `serve` — Start the server

```bash
crap-cms serve <CONFIG> [-d]
```

| Argument / Flag | Description |
|-----------------|-------------|
| `<CONFIG>` | Path to the config directory |
| `-d`, `--detach` | Run in the background (prints PID and exits) |

```bash
crap-cms serve ./my-project
crap-cms serve ./my-project -d
```

### `status` — Show project status

```bash
crap-cms status <CONFIG>
```

Prints collections (with row counts), globals, DB size, and migration status.

```bash
crap-cms status ./my-project
```

### `user` — User management

All user subcommands require a config directory as the first positional argument.

#### `user create`

```bash
crap-cms user create <CONFIG> [-c <COLLECTION>] [-e <EMAIL>] [-p <PASSWORD>] [-f <KEY=VALUE>]...
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--collection` | `-c` | `users` | Auth collection slug |
| `--email` | `-e` | — | User email (prompted if omitted) |
| `--password` | `-p` | — | User password (prompted if omitted) |
| `--field` | `-f` | — | Extra fields as key=value (repeatable) |

```bash
# Interactive (prompts for password)
crap-cms user create ./my-project -e admin@example.com

# Non-interactive
crap-cms user create ./my-project \
    -e admin@example.com \
    -p secret123 \
    -f role=admin \
    -f name="Admin User"
```

#### `user list`

```bash
crap-cms user list <CONFIG> [-c <COLLECTION>]
```

Lists all users with ID, email, locked status, and verified status (if email verification is enabled).

```bash
crap-cms user list ./my-project
crap-cms user list ./my-project -c admins
```

#### `user delete`

```bash
crap-cms user delete <CONFIG> [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Auth collection slug (default: `users`) |
| `--email` | `-e` | User email |
| `--id` | — | User ID |
| `--confirm` | `-y` | Skip confirmation prompt |

#### `user lock` / `user unlock`

```bash
crap-cms user lock <CONFIG> [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
crap-cms user unlock <CONFIG> [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>]
```

#### `user change-password`

```bash
crap-cms user change-password <CONFIG> [-c <COLLECTION>] [-e <EMAIL>] [--id <ID>] [-p <PASSWORD>]
```

### `init` — Scaffold a new config directory

```bash
crap-cms init [DIR]
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
crap-cms serve ./my-project
```

### `make` — Generate scaffolding files

#### `make collection`

```bash
crap-cms make collection <CONFIG> [SLUG] [-F <FIELDS>] [-T] [--auth] [--upload] [--versions] [--no-input] [-f]
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
crap-cms make collection ./my-project posts

# With fields
crap-cms make collection ./my-project articles \
    -F "title:text:required,body:richtext"

# With localized fields
crap-cms make collection ./my-project pages \
    -F "title:text:required:localized,body:textarea:localized,slug:text:required"

# Auth collection
crap-cms make collection ./my-project users --auth

# Upload collection
crap-cms make collection ./my-project media --upload

# Non-interactive with versions
crap-cms make collection ./my-project posts \
    -F "title:text:required,body:richtext" --versions --no-input
```

#### `make global`

```bash
crap-cms make global <CONFIG> <SLUG> [-f]
```

```bash
crap-cms make global ./my-project site_settings
```

#### `make hook`

```bash
crap-cms make hook <CONFIG> [NAME] [-t <TYPE>] [-c <COLLECTION>] [-l <POSITION>] [-F <FIELD>] [--force]
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
crap-cms make hook ./my-project

# Fully specified
crap-cms make hook ./my-project auto_slug \
    -t collection -c posts -l before_change

# Field hook
crap-cms make hook ./my-project normalize_email \
    -t field -c users -l before_validate -F email

# Access hook
crap-cms make hook ./my-project owner_only \
    -t access -c posts -l read

# Condition hook (client-side table)
crap-cms make hook ./my-project show_external_url \
    -t condition -c posts -l table -F post_type
```

#### `make job`

```bash
crap-cms make job <CONFIG> [SLUG] [-s <SCHEDULE>] [-q <QUEUE>] [-r <RETRIES>] [-t <TIMEOUT>] [-f]
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
crap-cms make job ./my-project

# With schedule
crap-cms make job ./my-project cleanup_expired -s "0 3 * * *" -r 3 -t 300

# Simple job (triggered from hooks)
crap-cms make job ./my-project send_welcome_email
```

### `blueprint` — Manage saved blueprints

#### `blueprint save`

```bash
crap-cms blueprint save <CONFIG> <NAME> [-f]
```

Saves a config directory as a reusable blueprint (excluding `data/`, `uploads/`, `types/`). A `.crap-blueprint.toml` manifest is written with the CMS version and timestamp.

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
crap-cms db console <CONFIG>
```

Opens an interactive `sqlite3` session on the project database.

#### `db cleanup`

```bash
crap-cms db cleanup <CONFIG> [--confirm]
```

| Flag | Description |
|------|-------------|
| `--confirm` | Actually drop orphan columns (default: dry-run report only) |

Detects columns in collection tables that don't correspond to any field in the current Lua definitions. System columns (`_`-prefixed like `_password_hash`, `_locked`) are always kept. Plugin columns are safe because plugins run during schema loading — their fields are part of the live definitions.

```bash
# Dry run — show orphans without removing them
crap-cms db cleanup ./my-project

# Actually drop orphan columns
crap-cms db cleanup ./my-project --confirm
```

### `export` — Export collection data

```bash
crap-cms export <CONFIG> [-c <COLLECTION>] [-o <FILE>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Export only this collection (default: all) |
| `--output` | `-o` | Output file (default: stdout) |

Export includes `crap_version` and `exported_at` metadata in the JSON envelope. On import, a version mismatch produces a warning (but does not abort).

```bash
crap-cms export ./my-project
crap-cms export ./my-project -c posts -o posts.json
```

### `import` — Import collection data

```bash
crap-cms import <CONFIG> <FILE> [-c <COLLECTION>]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Import only this collection (default: all in file) |

```bash
crap-cms import ./my-project backup.json
crap-cms import ./my-project backup.json -c posts
```

### `typegen` — Generate typed definitions

```bash
crap-cms typegen <CONFIG> [-l <LANG>] [-o <DIR>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--lang` | `-l` | `lua` | Output language: `lua`, `ts`, `go`, `py`, `rs`, `all` |
| `--output` | `-o` | `<config>/types/` | Output directory for generated files |

```bash
crap-cms typegen ./my-project
crap-cms typegen ./my-project -l all
crap-cms typegen ./my-project -l ts -o ./client/src/types
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
crap-cms migrate <CONFIG> <create|up|down|list|fresh>
```

| Subcommand | Description |
|------------|-------------|
| `create <NAME>` | Generate a new migration file (e.g., `backfill_slugs`) |
| `up` | Sync schema + run pending migrations |
| `down [-s N]` | Roll back last N migrations (default: 1) |
| `list` | Show all migration files with status |
| `fresh [-y\|--confirm]` | Drop all tables and recreate (destructive, requires confirmation) |

```bash
crap-cms migrate ./my-project create backfill_slugs
crap-cms migrate ./my-project up
crap-cms migrate ./my-project list
crap-cms migrate ./my-project down -s 2
crap-cms migrate ./my-project fresh -y
```

### `backup` — Backup database

```bash
crap-cms backup <CONFIG> [-o <DIR>] [-i]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--output` | `-o` | Output directory (default: `<config>/backups/`) |
| `--include-uploads` | `-i` | Also compress the uploads directory |

```bash
crap-cms backup ./my-project
crap-cms backup ./my-project -o /tmp/backups -i
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
crap-cms templates extract <CONFIG> [PATHS...] [-a] [-t <TYPE>] [-f]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--all` | `-a` | Extract all files |
| `--type` | `-t` | Filter: `templates` or `static` (only with `--all`) |
| `--force` | `-f` | Overwrite existing files |

```bash
# Extract specific files
crap-cms templates extract ./my-project layout/base.hbs styles.css

# Extract all templates
crap-cms templates extract ./my-project --all --type templates

# Extract everything, overwriting existing
crap-cms templates extract ./my-project --all --force
```

### `jobs` — Manage background jobs

All jobs subcommands require a config directory.

#### `jobs list`

```bash
crap-cms jobs list <CONFIG>
```

Lists all defined jobs with their configuration (handler, schedule, queue, retries, timeout, concurrency).

#### `jobs trigger`

```bash
crap-cms jobs trigger <CONFIG> <SLUG>
```

Manually queue a job for execution. Works even while the server is running (SQLite WAL allows concurrent access). Prints the queued job run ID.

#### `jobs status`

```bash
crap-cms jobs status <CONFIG> [--id <ID>]
```

Show recent job runs. If `--id` is given, shows details for that specific run. Otherwise lists recent runs across all jobs.

#### `jobs purge`

```bash
crap-cms jobs purge <CONFIG> [--older-than <DURATION>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--older-than` | `7d` | Delete completed/failed/stale runs older than this. Supports `Nd`, `Nh`, `Nm` formats. |

#### `jobs healthcheck`

```bash
crap-cms jobs healthcheck <CONFIG>
```

Checks job system health and prints a summary: defined jobs, stale jobs (running but heartbeat expired), failed jobs in the last 24 hours, pending jobs waiting longer than 5 minutes, and scheduled jobs that have never completed a run.

Exit status: `healthy` (no issues), `warning` (failed or long-pending jobs), `unhealthy` (stale jobs detected).

```bash
crap-cms jobs list ./my-project
crap-cms jobs trigger ./my-project cleanup_expired
crap-cms jobs status ./my-project
crap-cms jobs status ./my-project --id abc123
crap-cms jobs purge ./my-project --older-than 30d
crap-cms jobs healthcheck ./my-project
```

### `images` — Manage image processing queue

Inspect and manage the background image format conversion queue. See [Image Processing](../uploads/image-processing.md) for how to enable queued conversion.

#### `images list`

```bash
crap-cms images list <CONFIG> [-s <STATUS>] [-l <LIMIT>]
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--status` | `-s` | — | Filter by status: `pending`, `processing`, `completed`, `failed` |
| `--limit` | `-l` | `20` | Max entries to show |

#### `images stats`

```bash
crap-cms images stats <CONFIG>
```

Shows counts by status (pending, processing, completed, failed) and total.

#### `images retry`

```bash
crap-cms images retry <CONFIG> [--id <ID>] [--all] [-y]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--id` | — | Retry a specific failed entry by ID |
| `--all` | — | Retry all failed entries |
| `--confirm` | `-y` | Required with `--all` |

#### `images purge`

```bash
crap-cms images purge <CONFIG> [--older-than <DURATION>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--older-than` | `7d` | Delete completed/failed entries older than this. Supports `Nd`, `Nh`, `Nm`, `Ns` formats. |

```bash
crap-cms images list ./my-project
crap-cms images list ./my-project -s failed
crap-cms images stats ./my-project
crap-cms images retry ./my-project --id abc123
crap-cms images retry ./my-project --all -y
crap-cms images purge ./my-project --older-than 30d
```

### `mcp` — Start the MCP server (stdio)

Start an MCP (Model Context Protocol) server over stdio for AI assistant integration.

```bash
crap-cms mcp <CONFIG>
```

| Argument | Description |
|----------|-------------|
| `<CONFIG>` | Path to the config directory |

```bash
crap-cms mcp ./my-project
```

Reads JSON-RPC 2.0 from stdin, writes responses to stdout. Use with Claude Desktop,
Cursor, VS Code, or any MCP-compatible client. See [MCP Overview](../mcp/overview.md)
for configuration and usage.

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Controls log verbosity. Default: `crap_cms=debug,info`. Example: `RUST_LOG=crap_cms=trace` |
