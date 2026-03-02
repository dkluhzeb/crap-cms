--- Row label function for project content blocks.
---@param data table<string, any>
---@return string
return function(data)
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
end
