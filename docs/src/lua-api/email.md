# crap.email

Send emails via SMTP. Requires the `[email]` section in `crap.toml` to be configured.

## Configuration

```toml
[email]
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_user = "noreply@example.com"
smtp_pass = "your-smtp-password"
smtp_tls = "starttls"    # "starttls" (default), "tls" (implicit), "none" (plain/test)
from_address = "noreply@example.com"
from_name = "My App"
```

If `smtp_host` is empty (default), all `crap.email.send()` calls log a warning and return `true` (no-op). The system remains fully functional without SMTP.

## crap.email.send(opts)

Send an email.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject line |
| `html` | string | yes | HTML email body |
| `text` | string | no | Plain text fallback body |

**Returns:** `true` on success.

**Example:**

```lua
crap.email.send({
    to = "user@example.com",
    subject = "Welcome!",
    html = "<h1>Welcome</h1><p>Thanks for signing up.</p>",
    text = "Welcome! Thanks for signing up.",
})
```

### Use in Hooks

`crap.email.send()` is blocking (uses SMTP transport), which is correct because Lua hooks run inside `spawn_blocking`. Safe to call from any hook.

```lua
-- hooks/notifications.lua
local M = {}

function M.notify_on_create(ctx)
    local admin_email = crap.env.get("ADMIN_EMAIL")
    if admin_email then
        crap.email.send({
            to = admin_email,
            subject = "New " .. ctx.collection .. " created",
            html = "<p>A new document was created in <b>" .. ctx.collection .. "</b>.</p>",
        })
    end
    return ctx
end

return M
```

```lua
-- collections/posts.lua
crap.collections.define("posts", {
    hooks = {
        after_change = { "hooks.notifications.notify_on_create" },
    },
    fields = { ... },
})
```
