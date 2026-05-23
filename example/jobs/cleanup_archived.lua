--- Cron job: daily cleanup of archived inquiries older than 90 days.
local M = {}

M.run = crap.any.job_handler(function(_context)
  -- `os.date(fmt, time)` is typed as `string|osdate`. The bang prefix
  -- in the format string guarantees a string return, but LuaLS can't
  -- infer that — cast explicitly so the value type-checks downstream
  -- as a `crap.FilterScalar`.
  local cutoff = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time() - (90 * 24 * 60 * 60)) --[[@as string]]

  -- Per-collection accessor — `result` is `crap.find_result.Inquiries`.
  local result = crap.collections.inquiries.find({
    where = {
      status = "archived",
      created_at = { less_than = cutoff },
    },
    overrideAccess = true,
  })

  if not result or not result.documents then
    return
  end

  local count = 0
  for _, doc in ipairs(result.documents) do
    crap.collections.inquiries.delete(doc.id, { overrideAccess = true })
    count = count + 1
  end

  if count > 0 then
    crap.log.info(string.format("Cleaned up %d archived inquiries", count))
  end
end)

crap.jobs.define("cleanup_archived", {
  handler = "jobs.cleanup_archived.run",
  schedule = "0 3 * * *",
  -- Low priority: this is background maintenance. If the queue has a
  -- backlog of user-triggered work, let it drain first. Operators
  -- can enable `[jobs] priority_decay = "1h"` in crap.toml if they
  -- want this job to age into being claimable even during sustained
  -- high-priority traffic.
  priority = -5,
  labels = { singular = "Cleanup Archived Inquiries" },
})

return M
