# Display Conditions

Display conditions let you show or hide fields in the admin UI based on the values of other fields. This is useful for context-dependent forms — for example, showing a URL field only when the post type is "link".

## Configuration

Add `admin.condition` to a field definition, referencing a Lua function:

```lua
crap.fields.text({
    name = "external_url",
    admin = {
        condition = "hooks.posts.show_external_url",
    },
}),
```

The condition references a Lua function using the standard hook ref format (`hooks.<collection>.<name>`). The function receives the current form data and returns **either** a condition table (client-side) or a boolean (server-side).

The `data` parameter is typed per-collection (`crap.data.Posts`, `crap.global_data.SiteSettings`) for IDE autocomplete. The type generator emits these types automatically.

Use `crap-cms make hook` with `--type condition` to scaffold condition hooks:

```bash
crap-cms -C ./config make hook show_external_url \
    -t condition -c posts -l table -F post_type
```

## Condition Functions

### Client-Side (Condition Table)

When the function returns a **table**, it is serialized to JSON and embedded in the HTML. JavaScript evaluates it instantly on field changes — no server round-trip.

```lua
-- hooks/posts/show_external_url.lua
---@param data crap.data.Posts
---@return table
return function(data)
    return { field = "post_type", equals = "link" }
end
```

### Server-Side (Boolean)

When the function returns a **boolean**, the field visibility is re-evaluated on the server via a debounced fetch (300ms delay after the last input change). Use this for complex logic that can't be expressed as a simple condition table.

```lua
-- hooks/posts/show_premium_options.lua
---@param data crap.data.Posts
---@return boolean
return function(data)
    -- Complex logic that needs server-side evaluation
    local tags = data.tags or {}
    for _, tag in ipairs(tags) do
        if tag == "premium" then return true end
    end
    return false
end
```

> **Performance tip:** Prefer condition tables over booleans whenever possible. Tables evaluate instantly in the browser; booleans require a server round-trip on every field change.

## Condition Table Operators

| Operator | Example | Description |
|----------|---------|-------------|
| `equals` | `{ field = "type", equals = "link" }` | Exact match |
| `not_equals` | `{ field = "type", not_equals = "draft" }` | Not equal |
| `in` | `{ field = "type", ["in"] = {"link", "video"} }` | Value in list |
| `not_in` | `{ field = "type", not_in = {"a", "b"} }` | Value not in list |
| `is_truthy` | `{ field = "has_image", is_truthy = true }` | Non-empty, non-nil, non-false |
| `is_falsy` | `{ field = "has_image", is_falsy = true }` | Empty, nil, or false |

## Multiple Conditions (AND)

Return an array of condition tables to require all conditions to be true:

```lua
-- hooks/posts/show_advanced.lua
---@param data crap.data.Posts
---@return table
return function(data)
    return {
        { field = "post_type", not_equals = "link" },
        { field = "excerpt", is_truthy = true },
    }
end
```

## How It Works

### Page Load

1. The server calls the Lua condition function with the current document data
2. Based on the return type:
   - **Table:** serialized as a `data-condition` JSON attribute on the field wrapper; initial visibility computed server-side
   - **Boolean:** result sets initial visibility; function reference stored as `data-condition-ref`
3. Fields with `false` conditions render with `display: none` (no flash of content)

### Client-Side Reactivity (Condition Tables)

When the user changes a form field:
1. JavaScript reads the `data-condition` JSON from each conditional field
2. Evaluates the condition against current form values
3. Shows or hides the field instantly

### Server-Side Reactivity (Boolean Functions)

When the user changes a form field:
1. JavaScript debounces for 300ms
2. POSTs current form data to `/admin/collections/{slug}/evaluate-conditions`
3. Server calls each boolean condition function
4. Response updates field visibility

## Sidebar Fields

Display conditions work on fields in any position, including sidebar fields (`admin.position = "sidebar"`).

## Safe Defaults

- If a condition function throws an error, the field remains **visible** (safe default)
- If the condition returns `nil`, the field remains **visible**
- On page load, fields are hidden server-side before rendering (no flash)
