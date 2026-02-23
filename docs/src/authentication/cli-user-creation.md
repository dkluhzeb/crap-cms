# CLI User Creation

The `--create-user` flag bootstraps users without the admin UI or gRPC API. Useful for creating the first admin user.

## Interactive Mode

Prompts for password with hidden input and confirmation:

```bash
cargo run -- --config ./my-project --create-user --email admin@example.com
```

Output:

```
Password: ********
Confirm password: ********
Created user abc123 in 'users'
```

If required fields have no default value, you'll be prompted for those too.

## Non-Interactive Mode

For CI/scripting. The `--password` flag skips the prompt:

```bash
cargo run -- --config ./my-project --create-user \
    --email admin@example.com \
    --password secret123 \
    --field role=admin \
    --field name="Admin User"
```

> **Warning:** The password may be visible in shell history. Use interactive mode for production bootstrapping.

## Flags

| Flag | Description |
|------|-------------|
| `--create-user` | Enable user creation mode |
| `--collection <slug>` | Auth collection to create the user in (default: `users`) |
| `--email <email>` | User email (prompted if omitted) |
| `--password <password>` | User password (prompted if omitted) |
| `--field <key=value>` | Extra field values (repeatable) |

## Behavior

- Runs after Lua definitions are loaded and database schema is synced
- **No hooks are fired** (this is a bootstrap/admin tool)
- Creates the user in a single transaction
- Hashes the password with Argon2id
- Exits after creating the user (does not start the server)

## Field Handling

- Required fields with `default_value` — uses the default, prompts with `[default]` if interactive
- Required fields without defaults — prompts for input, fails if empty
- Optional fields — skipped unless provided via `--field`
- Checkbox fields — skipped (absent = false)
- Email field — always required (handled separately from `--field`)

## Examples

```bash
# Minimal (will prompt for everything else)
cargo run -- --config ./example --create-user

# Different collection
cargo run -- --config ./example --create-user --collection admins \
    --email boss@example.com

# Full non-interactive
cargo run -- --config ./example --create-user \
    --email editor@example.com \
    --password pass123 \
    --field name="Jane Editor" \
    --field role=editor
```
