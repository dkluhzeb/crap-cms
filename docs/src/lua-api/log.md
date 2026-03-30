# crap.log

Structured logging that maps to Rust's `tracing` framework. Log messages appear with a `[lua:<vm>]` prefix, where `<vm>` is the VM label (e.g., `init`, `vm-1`, `vm-2`).

## Functions

### crap.log.info(msg)

Log an info-level message.

```lua
crap.log.info("Processing complete")
```

Output: `INFO [lua:vm-1] Processing complete`

### crap.log.warn(msg)

Log a warning-level message.

```lua
crap.log.warn("Deprecated field used")
```

Output: `WARN [lua:vm-1] Deprecated field used`

### crap.log.error(msg)

Log an error-level message.

```lua
crap.log.error("Failed to process webhook")
```

Output: `ERROR [lua:vm-1] Failed to process webhook`

## Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `msg` | string | Log message |

## Usage in Hooks

```lua
function M.before_change(ctx)
    crap.log.info(string.format(
        "[%s] %s on %s",
        os.date("%H:%M:%S"),
        ctx.operation,
        ctx.collection
    ))
    return ctx
end
```
