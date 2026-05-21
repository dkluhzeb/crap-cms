--- Field before_validate hook: trim whitespace from the field value.
return crap.any.field_hook(function(value, _context)
  if type(value) == "string" then
    return value:match("^%s*(.-)%s*$")
  end
  return value
end)
