# Execution Order

At each lifecycle stage, hooks run in this order:

```
1. Field-level hooks (per-field, in field definition order)
2. Collection-level hooks (string refs from collection definition, in array order)
3. Globally registered hooks (from crap.hooks.register(), in registration order)
```

## Example: before_change on create

Given this setup:

```lua
-- collections/posts.lua
crap.collections.define("posts", {
    fields = {
        crap.fields.text({
            name = "title",
            hooks = { before_change = { "hooks.fields.uppercase" } },
        }),
        crap.fields.text({
            name = "slug",
            hooks = { before_change = { "hooks.fields.normalize_slug" } },
        }),
    },
    hooks = {
        before_change = { "hooks.posts.set_defaults", "hooks.posts.validate_business_rules" },
    },
})

-- init.lua
crap.hooks.register("before_change", function(ctx)
    crap.log.info("audit: " .. ctx.operation .. " on " .. ctx.collection)
    return ctx
end)
```

Execution order for a `create` operation:

1. `hooks.fields.uppercase` (field: title)
2. `hooks.fields.normalize_slug` (field: slug)
3. `hooks.posts.set_defaults` (collection)
4. `hooks.posts.validate_business_rules` (collection)
5. Registered audit function (global)

## Multiple Hooks per Event

Both field-level and collection-level hooks accept arrays. Hooks in the same array run sequentially, each receiving the output of the previous one.

```lua
hooks = {
    before_change = { "hooks.posts.first", "hooks.posts.second" },
}
```

`first` runs, its returned context is passed to `second`.
