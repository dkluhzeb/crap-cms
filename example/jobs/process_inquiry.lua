--- Queued job: send email notification + webhook for new inquiries.
local M = {}

--- Data shape passed from the notify_inquiry hook via crap.jobs.queue().
---@class ProcessInquiryData
---@field inquiry_id string
---@field name string
---@field email string
---@field service? string

---@param context crap.JobHandlerContext
function M.run(context)
  ---@type ProcessInquiryData
  local data = context.data
  local inquiry_id = data and data.inquiry_id
  if not inquiry_id then
    crap.log.error("process_inquiry: missing inquiry_id")
    return
  end

  ---@type crap.doc.Inquiries?
  local inquiry = crap.collections.find_by_id("inquiries", inquiry_id)
  if not inquiry then
    crap.log.warn("process_inquiry: inquiry not found: " .. inquiry_id)
    return
  end

  -- Send email notification
  crap.email.send({
    to = "hello@crap.studio",
    subject = string.format("New inquiry from %s", inquiry.name or "Unknown"),
    html = string.format(
      "<h2>New Inquiry</h2>"
        .. "<p><strong>From:</strong> %s (%s)</p>"
        .. "<p><strong>Company:</strong> %s</p>"
        .. "<p><strong>Budget:</strong> %s</p>"
        .. "<p><strong>Message:</strong></p><p>%s</p>",
      inquiry.name or "",
      inquiry.email or "",
      inquiry.company or "N/A",
      inquiry.budget_range or "Not specified",
      inquiry.message or ""
    ),
  })

  -- Send webhook notification
  local webhook_url = crap.env.get("CRAP_INQUIRY_WEBHOOK_URL")
  if webhook_url then
    local ok, err = pcall(function()
      crap.http.request({
        method = "POST",
        url = webhook_url,
        headers = {
          ["Content-Type"] = "application/json",
        },
        body = crap.json.encode({
          event = "new_inquiry",
          inquiry_id = inquiry_id,
          name = inquiry.name,
          email = inquiry.email,
          company = inquiry.company,
          budget_range = inquiry.budget_range,
        }),
      })
    end)
    if not ok then
      crap.log.warn("process_inquiry: webhook failed: " .. tostring(err))
    end
  end

  -- Update status to contacted
  crap.collections.update("inquiries", inquiry_id, {
    status = "contacted",
  })

  crap.log.info("Processed inquiry: " .. inquiry_id)
end

crap.jobs.define("process_inquiry", {
  handler = "jobs.process_inquiry.run",
  queue = "notifications",
  retries = 3,
  timeout = 30,
  labels = { singular = "Process Inquiry" },
})

return M
