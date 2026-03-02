--- Collection after_change hook: queue email notification for new inquiries.
---@param context crap.hook.Inquiries
---@return crap.hook.Inquiries
return function(context)
  if context.operation ~= "create" then
    return context
  end

  local data = context.data
  if not data then
    return context
  end

  crap.jobs.queue("process_inquiry", {
    inquiry_id = data.id,
    name = data.name,
    email = data.email,
    service = data.service,
  })

  crap.log.info(string.format("Inquiry from %s queued for processing", data.email or "unknown"))

  return context
end
