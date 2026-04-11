# crap.email

Send emails via the configured email provider. Supports SMTP, webhooks (SendGrid, Mailgun, etc.), and custom Lua providers.

## Configuration

```toml
[email]
provider = "smtp"        # "smtp" (default), "webhook", "log", or "custom"
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_user = "noreply@example.com"
smtp_pass = "your-smtp-password"
smtp_tls = "starttls"    # "starttls" (default), "tls" (implicit), "none" (plain/test)
from_address = "noreply@example.com"
from_name = "My App"

# Queue settings for crap.email.queue()
queue_retries = 3        # retry count (default: 3)
queue_name = "email"     # job queue name (default: "email")
queue_timeout = 30       # per-attempt timeout in seconds (default: 30)
queue_concurrency = 5    # max concurrent queued emails (default: 5)
```

If `smtp_host` is empty with the default `smtp` provider, emails are logged instead of sent (equivalent to `provider = "log"`).

## crap.email.send(opts)

Send an email immediately (blocking). Use when you need to know if the send succeeded.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject line |
| `html` | string | yes | HTML email body |
| `text` | string | no | Plain text fallback body |

**Returns:** `true` on success.

```lua
crap.email.send({
    to = "user@example.com",
    subject = "Welcome!",
    html = "<h1>Welcome</h1><p>Thanks for signing up.</p>",
    text = "Welcome! Thanks for signing up.",
})
```

## crap.email.queue(opts)

Queue an email for async delivery with automatic retries. Returns immediately — the email is processed by the scheduler with exponential backoff on failure (5s, 10s, 20s, ..., max 300s).

Requires a transaction context (available in `before_change`, `before_delete`, and other hooks with CRUD access).

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject line |
| `html` | string | yes | HTML email body |
| `text` | string | no | Plain text fallback body |
| `retries` | integer | no | Override retry count from config (default: `email.queue_retries`) |

**Returns:** Job run ID (string).

```lua
local job_id = crap.email.queue({
    to = "user@example.com",
    subject = "Your report is ready",
    html = "<p>Download your report at ...</p>",
    retries = 5,  -- try up to 5 times
})
```

> **When to use `send` vs `queue`:**
> - Use `send` when the email result matters for the current operation (e.g., inline error handling).
> - Use `queue` for everything else — password resets, notifications, reports. The queue handles retries automatically, and failures don't block the request.

## crap.email.register(handler)

Register a custom email provider. Only used when `[email] provider = "custom"` in `crap.toml`. Call in `init.lua`.

```lua
crap.email.register({
    send = function(opts)
        crap.http.request({
            method = "POST",
            url = "https://api.sendgrid.com/v3/mail/send",
            headers = {
                Authorization = "Bearer " .. crap.env.get("SENDGRID_KEY"),
                ["Content-Type"] = "application/json",
            },
            body = crap.json.encode({
                personalizations = {{ to = {{ email = opts.to }} }},
                from = { email = "noreply@example.com" },
                subject = opts.subject,
                content = {{ type = "text/html", value = opts.html }},
            }),
        })
    end,
})
```

## Providers

| Provider | Config | Description |
|----------|--------|-------------|
| `smtp` | `smtp_host`, `smtp_port`, etc. | Default. Standard SMTP via `lettre`. |
| `webhook` | `webhook_url`, `webhook_headers` | HTTP POST with JSON body. Works with SendGrid, Mailgun, Resend. |
| `log` | (none) | Logs emails to tracing. For development/testing. |
| `custom` | (none) | Delegates to Lua via `crap.email.register()`. |

## Use in Hooks

Both `send` and `queue` are safe to call from hooks. `send` is blocking (runs within `spawn_blocking`). `queue` inserts a job row and returns immediately.

```lua
-- hooks/welcome.lua — queue a welcome email on user creation
return function(ctx)
    if ctx.operation == "create" and ctx.data.email then
        crap.email.queue({
            to = ctx.data.email,
            subject = "Welcome!",
            html = "<p>Thanks for signing up.</p>",
        })
    end
    return ctx
end
```
