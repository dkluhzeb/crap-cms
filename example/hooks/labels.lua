--- Row label function for projects content blocks. Lives on the
--- per-collection accessor for discoverability — the row table
--- shape is generic (`table<string, any>`) because content blocks
--- can hold any block type.
return crap.collections.projects.row_label(function(data)
  local block_type = data._block_type or "block"
  local label = data.heading or data.title or data.caption or ""

  if label == "" then
    return block_type
  end

  -- Truncate long labels
  if #label > 50 then
    label = label:sub(1, 47) .. "..."
  end

  return string.format("%s: %s", block_type, label)
end)
