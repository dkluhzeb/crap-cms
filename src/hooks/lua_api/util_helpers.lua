local util = crap.util

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

function util.pick(tbl, keys)
    local out = {}
    for _, k in ipairs(keys) do
        out[k] = tbl[k]
    end
    return out
end

function util.omit(tbl, keys)
    local skip = {}
    for _, k in ipairs(keys) do skip[k] = true end
    local out = {}
    for k, v in pairs(tbl) do
        if not skip[k] then out[k] = v end
    end
    return out
end

function util.keys(tbl)
    local out = {}
    for k in pairs(tbl) do out[#out + 1] = k end
    return out
end

function util.values(tbl)
    local out = {}
    for _, v in pairs(tbl) do out[#out + 1] = v end
    return out
end

function util.map(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do out[i] = fn(v, i) end
    return out
end

function util.filter(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do
        if fn(v, i) then out[#out + 1] = v end
    end
    return out
end

function util.find(tbl, fn)
    for i, v in ipairs(tbl) do
        if fn(v, i) then return v end
    end
    return nil
end

function util.includes(tbl, value)
    for _, v in ipairs(tbl) do
        if v == value then return true end
    end
    return false
end

function util.is_empty(tbl)
    return next(tbl) == nil
end

function util.clone(tbl)
    local out = {}
    for k, v in pairs(tbl) do out[k] = v end
    return out
end

function util.trim(str)
    return (str:gsub("^%s+", ""):gsub("%s+$", ""))
end

function util.split(str, sep)
    local out = {}
    local pattern = "([^" .. sep .. "]+)"
    for part in str:gmatch(pattern) do
        out[#out + 1] = part
    end
    return out
end

function util.starts_with(str, prefix)
    return str:sub(1, #prefix) == prefix
end

function util.ends_with(str, suffix)
    return suffix == "" or str:sub(-#suffix) == suffix
end

function util.truncate(str, max_len, suffix)
    suffix = suffix or "..."
    if #str <= max_len then return str end
    return str:sub(1, max_len - #suffix) .. suffix
end
