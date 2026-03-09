# CLI User Creation

The `user create` command bootstraps users without the admin UI or gRPC API. Useful for creating the first admin user.

## Interactive Mode

Prompts for password with hidden input and confirmation:

```bash
crap-cms user create ./my-project -e admin@example.com
```

Output:

```
Password: ********
Confirm password: ********
Created user abc123 in 'users'
```

If required fields have no default value, you'll be prompted for those too.

## Non-Interactive Mode

For CI/scripting. The `-p` flag skips the prompt:

```bash
crap-cms user create ./my-project \
    -e admin@example.com \
    -p secret123 \
    -f role=admin \
    -f name="Admin User"
```

> **Warning:** The password may be visible in shell history. Use interactive mode for production bootstrapping.

## Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Auth collection to create the user in (default: `users`) |
| `--email` | `-e` | User email (prompted if omitted) |
| `--password` | `-p` | User password (prompted if omitted) |
| `--field` | `-f` | Extra field values as key=value (repeatable) |

## Behavior

- Runs after Lua definitions are loaded and database schema is synced
- **No hooks are fired** (this is a bootstrap/admin tool)
- Creates the user in a single transaction
- Hashes the password with Argon2id
- Exits after creating the user (does not start the server)

## Field Handling

- Required fields with `default_value` — uses the default, prompts with `[default]` if interactive
- Required fields without defaults — prompts for input, fails if empty
- Optional fields — skipped unless provided via `-f`
- Checkbox fields — skipped (absent = false)
- Email field — always required (handled separately from `-f`)

## Examples

```bash
# Minimal (will prompt for everything else)
crap-cms user create ./example

# Different collection
crap-cms user create ./example -c admins \
    -e boss@example.com

# Full non-interactive
crap-cms user create ./example \
    -e editor@example.com \
    -p pass123 \
    -f name="Jane Editor" \
    -f role=editor
```

## Other User Commands

```bash
# Show detailed info for a user
crap-cms user info ./example -e admin@example.com

# List all users
crap-cms user list ./example

# Lock/unlock a user
crap-cms user lock ./example -e user@example.com
crap-cms user unlock ./example -e user@example.com

# Verify/unverify a user (requires verify_email: true on collection)
crap-cms user verify ./example -e user@example.com
crap-cms user unverify ./example -e user@example.com

# Change password
crap-cms user change-password ./example -e user@example.com

# Delete a user (with confirmation skip)
crap-cms user delete ./example -e user@example.com -y
```
