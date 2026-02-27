--- Field-level hooks.
local M = {}

--- before_change hook for the slug field: generate slug from title.
function M.slugify_title(value, ctx)
    -- If no slug provided, generate from title
    if value == nil or value == "" then
        if ctx.data and ctx.data.title then
            local s = ctx.data.title:lower()
            s = s:gsub("[^%w%s-]", "")
            s = s:gsub("%s+", "-")
            s = s:gsub("-+", "-")
            s = s:match("^%-*(.-)%-*$") or ""
            return s
        end
    end
    return value
end

--- after_read hook for a field: uppercases the value.
function M.uppercase_value(value, ctx)
    if type(value) == "string" then
        return value:upper()
    end
    return value
end

--- after_change hook for a field: logs and returns the value unmodified.
function M.after_change_marker(value, ctx)
    -- Just return the value with a marker suffix to prove it ran
    if type(value) == "string" then
        return value .. "_after_changed"
    end
    return value
end

--- before_validate hook for a field: trims whitespace.
function M.trim_value(value, ctx)
    if type(value) == "string" then
        return value:match("^%s*(.-)%s*$")
    end
    return value
end

--- Row label function for array/blocks rows.
function M.row_label(row)
    if row and row.label then
        return "Row: " .. row.label
    end
    return nil
end

--- Display condition function returning a boolean.
function M.show_if_published(data)
    if data and data.status == "published" then
        return true
    end
    return false
end

--- Display condition function returning a condition table.
function M.condition_table(data)
    return { field = "status", equals = "published" }
end

--- System hook (called via run_system_hooks_with_conn).
function M.system_init(ctx)
    -- System hooks get context but don't need to return anything useful.
    -- This just proves the hook was called successfully.
    return ctx
end

return M
