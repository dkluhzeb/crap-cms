--- Cron job: daily cleanup of archived inquiries older than 90 days.
local M = {}

---@param context crap.JobHandlerContext
function M.run(context)
  local cutoff = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time() - (90 * 24 * 60 * 60))

  ---@type crap.find_result.Inquiries
  local result = crap.collections.find("inquiries", {
    filters = {
      status = "archived",
      created_at = { less_than = cutoff },
    },
  })

  if not result or not result.documents then
    return
  end

  local count = 0
  for _, doc in ipairs(result.documents) do
    crap.collections.delete("inquiries", doc.id)
    count = count + 1
  end

  if count > 0 then
    crap.log.info(string.format("Cleaned up %d archived inquiries", count))
  end
end

crap.jobs.define("cleanup_archived", {
  handler = "jobs.cleanup_archived.run",
  schedule = "0 3 * * *",
  labels = { singular = "Cleanup Archived Inquiries" },
})

return M
