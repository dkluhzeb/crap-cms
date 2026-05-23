--- Collection after_change hook for inquiries: queue an email
--- notification when a new inquiry is created. Per-collection
--- factory narrows `context` to `crap.hook.Inquiries`.
return crap.collections.inquiries.hook(function(context)
  if context.operation ~= "create" then
    return context
  end

  local data = context.data
  if not data then
    return context
  end

  -- `unique = "process:" .. id` is idempotent: if a double-submit or
  -- replay fires this hook twice for the same inquiry, the second
  -- `queue()` returns the existing job id instead of inserting a
  -- duplicate. The email + webhook only fire once.
  crap.jobs.queue("process_inquiry", {
    inquiry_id = data.id,
    name = data.name,
    email = data.email,
    service = data.service,
  }, {
    unique = "process_inquiry:" .. data.id,
  })

  crap.log.info(string.format("Inquiry from %s queued for processing", data.email or "unknown"))

  return context
end)
