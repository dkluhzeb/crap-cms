local util = crap.util

-- @typegen-start crap.util — table helpers

--- Deep merge two tables. b overwrites a. Returns a new table.
--- @param a table  Base table.
--- @param b table  Override table.
--- @return table merged
function util.deep_merge(a, b)
    local out = {}
    for k, v in pairs(a) do
        out[k] = v
    end
    for k, v in pairs(b) do
        if type(out[k]) == "table" and type(v) == "table" then
            out[k] = util.deep_merge(out[k], v)
        else
            out[k] = v
        end
    end
    return out
end

--- Return a table with only the listed keys.
--- @param tbl table        Source table.
--- @param keys string[]    Keys to keep.
--- @return table
function util.pick(tbl, keys)
    local out = {}
    for _, k in ipairs(keys) do
        out[k] = tbl[k]
    end
    return out
end

--- Return a table without the listed keys.
--- @param tbl table        Source table.
--- @param keys string[]    Keys to remove.
--- @return table
function util.omit(tbl, keys)
    local skip = {}
    for _, k in ipairs(keys) do skip[k] = true end
    local out = {}
    for k, v in pairs(tbl) do
        if not skip[k] then out[k] = v end
    end
    return out
end

--- Extract all keys from a table as an array.
--- @param tbl table
--- @return string[]
function util.keys(tbl)
    local out = {}
    for k in pairs(tbl) do out[#out + 1] = k end
    return out
end

--- Extract all values from a table as an array.
--- @param tbl table
--- @return any[]
function util.values(tbl)
    local out = {}
    for _, v in pairs(tbl) do out[#out + 1] = v end
    return out
end

--- Map a function over an array table.
--- @param tbl any[]              Array to map over.
--- @param fn  fun(v: any, i: integer): any  Mapping function.
--- @return any[]
function util.map(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do out[i] = fn(v, i) end
    return out
end

--- Filter an array table by a predicate.
--- @param tbl any[]              Array to filter.
--- @param fn  fun(v: any, i: integer): boolean  Predicate function.
--- @return any[]
function util.filter(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do
        if fn(v, i) then out[#out + 1] = v end
    end
    return out
end

--- Find the first element matching a predicate.
--- @param tbl any[]              Array to search.
--- @param fn  fun(v: any, i: integer): boolean  Predicate function.
--- @return any?
function util.find(tbl, fn)
    for i, v in ipairs(tbl) do
        if fn(v, i) then return v end
    end
    return nil
end

--- Check if an array contains a value.
--- @param tbl any[]  Array to search.
--- @param value any  Value to find.
--- @return boolean
function util.includes(tbl, value)
    for _, v in ipairs(tbl) do
        if v == value then return true end
    end
    return false
end

--- Check if a table has no entries.
--- @param tbl table
--- @return boolean
function util.is_empty(tbl)
    return next(tbl) == nil
end

--- Shallow copy a table.
--- @param tbl table
--- @return table
function util.clone(tbl)
    local out = {}
    for k, v in pairs(tbl) do out[k] = v end
    return out
end

-- @typegen-end

-- @typegen-start crap.util — string helpers

--- Strip leading and trailing whitespace.
--- @param str string
--- @return string
function util.trim(str)
    return (str:gsub("^%s+", ""):gsub("%s+$", ""))
end

--- Split a string by separator.
--- @param str string  Input string.
--- @param sep string  Separator string.
--- @return string[]
function util.split(str, sep)
    local out = {}
    local pattern = "([^" .. sep .. "]+)"
    for part in str:gmatch(pattern) do
        out[#out + 1] = part
    end
    return out
end

--- Check if a string starts with a prefix.
--- @param str string
--- @param prefix string
--- @return boolean
function util.starts_with(str, prefix)
    return str:sub(1, #prefix) == prefix
end

--- Check if a string ends with a suffix.
--- @param str string
--- @param suffix string
--- @return boolean
function util.ends_with(str, suffix)
    return suffix == "" or str:sub(-#suffix) == suffix
end

--- Truncate a string to a max length with optional suffix.
--- @param str string       Input string.
--- @param max_len integer  Maximum length.
--- @param suffix? string   Suffix to append when truncated (default: "...").
--- @return string
function util.truncate(str, max_len, suffix)
    suffix = suffix or "..."
    if #str <= max_len then return str end
    return str:sub(1, max_len - #suffix) .. suffix
end

-- @typegen-end
