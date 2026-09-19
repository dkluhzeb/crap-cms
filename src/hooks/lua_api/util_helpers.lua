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

--- Split a string by a plain (non-pattern) separator. A multi-character
--- separator splits on the whole sequence; empty pieces are omitted.
--- @param str string  Input string.
--- @param sep string  Separator string (must be non-empty).
--- @return string[]
function util.split(str, sep)
    if sep == "" then
        error("crap.util.split: separator must be a non-empty string", 2)
    end
    local out = {}
    local init = 1
    while true do
        local s, e = string.find(str, sep, init, true)
        local piece = s and str:sub(init, s - 1) or str:sub(init)
        if piece ~= "" then
            out[#out + 1] = piece
        end
        if not s then
            return out
        end
        init = e + 1
    end
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

-- Length in characters (UTF-8 aware; bytes for a string that is not valid
-- UTF-8) and the prefix of the first `n` characters, on the same rule.
local function char_len(s)
    return utf8.len(s) or #s
end

local function char_prefix(s, n)
    if not utf8.len(s) then
        return s:sub(1, n)
    end
    return s:sub(1, utf8.offset(s, n + 1) - 1)
end

--- Truncate a string to at most `max_len` characters (not bytes), appending
--- `suffix` when it was cut. The result is never longer than `max_len`
--- characters — a suffix longer than `max_len` is itself cut to fit.
--- @param str string       Input string.
--- @param max_len integer  Maximum length in characters.
--- @param suffix? string   Suffix to append when truncated (default: "...").
--- @return string
function util.truncate(str, max_len, suffix)
    suffix = suffix or "..."
    if max_len < 0 then max_len = 0 end
    if char_len(str) <= max_len then return str end
    local keep = max_len - char_len(suffix)
    if keep < 0 then
        return char_prefix(suffix, max_len)
    end
    return char_prefix(str, keep) .. suffix
end

-- @typegen-end
