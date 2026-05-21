--- Cron job: weekly content report logged to stdout.
local M = {}

M.run = crap.any.job_handler(function(_context)
  -- Per-collection accessors give full return-type narrowing and
  -- per-column `where` autocomplete without any `---@type` ceremony.
  local posts = crap.collections.posts.find({ limit = 0, overrideAccess = true })
  local projects = crap.collections.projects.find({ limit = 0, overrideAccess = true })
  local inquiries = crap.collections.inquiries.find({
    where = { status = "new" },
    limit = 0,
    overrideAccess = true,
  })

  crap.log.info(
    string.format(
      "[Weekly Report] Posts: %d, Projects: %d, Open inquiries: %d",
      posts and posts.pagination.totalDocs or 0,
      projects and projects.pagination.totalDocs or 0,
      inquiries and inquiries.pagination.totalDocs or 0
    )
  )
end)

crap.jobs.define("weekly_report", {
  handler = "jobs.weekly_report.run",
  schedule = "0 9 * * 1",
  labels = { singular = "Weekly Report" },
})

return M
