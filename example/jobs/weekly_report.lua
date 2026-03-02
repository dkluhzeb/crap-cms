--- Cron job: weekly content report logged to stdout.
local M = {}

---@param context crap.JobHandlerContext
function M.run(context)
  ---@type crap.find_result.Posts
  local posts = crap.collections.find("posts", { limit = 0 })
  ---@type crap.find_result.Projects
  local projects = crap.collections.find("projects", { limit = 0 })
  ---@type crap.find_result.Inquiries
  local inquiries = crap.collections.find("inquiries", {
    filters = { status = "new" },
    limit = 0,
  })

  crap.log.info(
    string.format(
      "[Weekly Report] Posts: %d, Projects: %d, Open inquiries: %d",
      posts and posts.total or 0,
      projects and projects.total or 0,
      inquiries and inquiries.total or 0
    )
  )
end

crap.jobs.define("weekly_report", {
  handler = "jobs.weekly_report.run",
  schedule = "0 9 * * 1",
  labels = { singular = "Weekly Report" },
})

return M
