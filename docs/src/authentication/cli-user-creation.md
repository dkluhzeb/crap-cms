# CLI User Creation

The `user create` command bootstraps users without the admin UI or gRPC API. Useful for creating the first admin user.

## Interactive Mode

Prompts for password with hidden input and confirmation:

```bash
crap-cms user create -e admin@example.com
```

Output:

```
Password: ********
Confirm password: ********
Created user abc123 in 'users'
```

If required fields have no default value, you'll be prompted for those too —
including a required field inside a `row`, `collapsible` or `tabs` wrapper, which
is filled exactly like a top-level field. The `init` wizard's first-user step
uses the same prompts.

## Non-Interactive Mode

For CI/scripting, pass the password on standard input with `--password-stdin`
(the first line is the password):

```bash
printf '%s\n' "$ADMIN_PASSWORD" | crap-cms user create \
    -e admin@example.com \
    --password-stdin \
    -f role=admin \
    -f name="Admin User"
```

`-p <PASSWORD>` also skips the prompt, but a command-line argument is visible to
other local users (in the process list) and is kept in shell history; the command
prints a warning when you use it.

## Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--collection` | `-c` | Auth collection to create the user in (default: `users`) |
| `--email` | `-e` | User email (prompted if omitted) |
| `--password` | `-p` | User password (prompted if omitted). Visible in the process list and shell history |
| `--password-stdin` | — | Read the password from the first line of standard input. Conflicts with `-p` |
| `--field` | `-f` | Extra field values as key=value (repeatable) |

## Behavior

- Runs after Lua definitions are loaded and database schema is synced
- Creates the user through the same service write as the admin UI and the API, in a single transaction: field validation, `[auth.password_policy]`, has-many and array values, the version snapshot, reference counting, the search index and the live event all apply. On a collection with `verify_email`, the verification email is queued
- **Lifecycle hooks don't run** — no `before_*`/`after_*` collection or field hooks (this is a bootstrap/admin tool: the first user is created before any hook can rely on one). Validation still runs
- **The live event is still published**, like every other write: the collection's `live` filter and its `before_broadcast` hooks run on it before it reaches subscribers
- Collection access rules don't apply to the operator's CLI
- Hashes the password with Argon2id
- Exits after creating the user (does not start the server)

## Field Handling

- Layout wrappers (`row`, `collapsible`, `tabs`) are transparent: a field inside one is prompted for and passed with `-f` by its own name
- Required fields with `default_value` — uses the default, prompts with `[default]` if interactive
- Required fields without defaults — prompts for input, fails if empty
- Groups holding a required sub-field — prompted as one JSON object (`address (required, JSON object)`)
- Prompts for array/blocks, group and list fields name the JSON format they take (`JSON array of rows`, `JSON object`, `JSON array`)
- Optional fields — skipped unless provided via `-f`; an optional field with `default_value` gets its default
- Checkbox fields — skipped (absent = false)
- Email field — always required (handled separately from `-f`)
- Array, blocks and group fields take JSON: `-f links='[{"url":"https://example.com"}]'`. A value that isn't JSON is an error
- List fields (has-many relationships, uploads and `has_many` text/number/select) take a JSON array: `-f roles='["admin","editor"]'`. A has-many relationship or upload also takes comma-separated ids: `-f teams=t1,t2`
- Every other value is text; the write converts it to the field's type (`-f age=42`) and validates it

## Examples

```bash
# Minimal (will prompt for everything else)
crap-cms user create

# Different collection
crap-cms user create -c admins \
    -e boss@example.com

# Full non-interactive
crap-cms user create \
    -e editor@example.com \
    -p pass123 \
    -f name="Jane Editor" \
    -f role=editor
```

## Other User Commands

```bash
# Show detailed info for a user
crap-cms user info -e admin@example.com

# List all users
crap-cms user list

# Lock/unlock a user
crap-cms user lock -e user@example.com
crap-cms user unlock -e user@example.com

# Verify/unverify a user (requires verify_email: true on collection)
crap-cms user verify -e user@example.com
crap-cms user unverify -e user@example.com

# Change password
crap-cms user change-password -e user@example.com

# Reset TOTP enrollment (requires mfa = "totp" on the collection)
crap-cms user reset-totp -e user@example.com

# Delete a user (with confirmation skip)
crap-cms user delete -e user@example.com -y
```
