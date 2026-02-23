# Command-Line Flags

```
crap-cms [OPTIONS]
```

## Flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--config <PATH>` | path | `./crap-cms` | Path to the config directory |
| `--generate-types` | flag | — | Generate Lua type definitions and exit |
| `--create-user` | flag | — | Create a user in an auth collection and exit |
| `--collection <SLUG>` | string | `users` | Auth collection slug (used with `--create-user`) |
| `--email <EMAIL>` | string | — | User email (used with `--create-user`) |
| `--password <PASSWORD>` | string | — | User password (used with `--create-user`; omit for interactive prompt) |
| `--field <KEY=VALUE>` | repeatable | — | Extra field values (used with `--create-user`) |

## Usage Patterns

### Run the server

```bash
cargo run -- --config ./my-project
```

### Generate Lua type definitions

Writes `types/crap.lua` with LuaLS annotations and exits:

```bash
cargo run -- --config ./my-project --generate-types
```

### Create a user (interactive)

Prompts for password with confirmation (hidden input):

```bash
cargo run -- --config ./my-project --create-user --email admin@example.com
```

If required fields have no default, you'll be prompted for those too.

### Create a user (non-interactive)

For CI/scripting. Warns about shell history exposure:

```bash
cargo run -- --config ./my-project --create-user \
    --email admin@example.com \
    --password secret123 \
    --field role=admin \
    --field name="Admin User"
```

### Create a user in a different auth collection

```bash
cargo run -- --config ./my-project --create-user \
    --collection admins \
    --email admin@example.com
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Controls log verbosity. Default: `crap_cms=debug,info`. Example: `RUST_LOG=crap_cms=trace` |
