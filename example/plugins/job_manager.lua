--- Job manager plugin: an admin page for the background job system.
---
--- A proof of concept that the admin UI can be extended to cover a whole
--- subsystem without touching the CMS itself. Everything here is public
--- plugin surface: a custom page for the view, `crap.template_data` for the
--- data, and two custom routes for the actions.
---
--- What it does NOT do, deliberately: purge (bulk and irreversible — that
--- stays a CLI command with its explicit confirmation flag) and triggering
--- with a payload (a free-text JSON box is a new input surface worth
--- deciding on its own).
---
--- Install from `init.lua`:
---   require("plugins.job_manager").install({ access = "access.admin_only" })

local M = {}

--- How many recent runs the page shows.
local RUN_LIMIT = 25

--- Where the page lives, for the action routes to send the browser back to.
local PAGE_URL = "/admin/p/jobs"

--- Mount path for the action routes. `/admin` is reserved by the built-in
--- router, so plugin routes live outside it.
local ACTION_PREFIX = "/tools/jobs"

--- Shorten a value for a table cell. Counts UTF-8 characters rather than
--- bytes, so a cut never lands inside one.
---@param s string|nil
---@param max integer
---@return string
local function truncate(s, max)
	if not s or s == "" then
		return ""
	end

	local chars = {}
	for ch in s:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
		chars[#chars + 1] = ch
		if #chars > max then
			return table.concat(chars, "", 1, max) .. "…"
		end
	end

	return s
end

--- Describe one job for the template. Handlebars has no expression language,
--- so anything the view needs as a decision is decided here.
---@param job crap.JobDefinitionInfo
---@return table
local function job_row(job)
	return {
		slug = job.slug,
		name = job.label or job.slug,
		queue = job.queue,
		schedule = job.schedule or "manual",
		scheduled = job.schedule ~= nil,
		retries = job.retries,
		timeout = job.timeout,
		concurrency = job.concurrency,
	}
end

--- Describe one run for the template.
---@param run table
---@return table
local function run_row(run)
	return {
		id = run.id,
		slug = run.slug,
		status = run.status,
		is_failed = run.status == "failed",
		is_running = run.status == "running",
		is_pending = run.status == "pending",
		cancellable = run.status == "pending",
		attempt = string.format("%d/%d", run.attempt, run.max_attempts),
		created_at = run.created_at or "-",
		error = truncate(run.error, 120),
	}
end

--- Page data: the defined jobs and the most recent runs.
---
--- Both reads are access-gated by the job's own `access` rule, so a job this
--- admin may not read is absent from the list and its runs are absent from
--- the table — the page inherits the gate rather than re-implementing it.
---@param _ctx crap.template_ctx
---@return table
local function page_data(_ctx)
	local jobs = {}
	for _, job in ipairs(crap.jobs.list()) do
		jobs[#jobs + 1] = job_row(job)
	end

	local runs = {}
	local page = crap.jobs.list_runs({ limit = RUN_LIMIT })
	for _, run in ipairs(page.runs) do
		runs[#runs + 1] = run_row(run)
	end

	return {
		jobs = jobs,
		runs = runs,
		total_runs = page.total,
		has_jobs = #jobs > 0,
		has_runs = #runs > 0,
		trigger_url = ACTION_PREFIX .. "/trigger",
		cancel_url = ACTION_PREFIX .. "/cancel",
	}
end

--- Register the page, its data, and the action routes.
---
--- `access` gates all three the same way: the page (hidden from the sidebar
--- for anyone it denies) and both actions. Pass a hook ref, e.g.
--- `"access.admin_only"`.
---@param opts? { access: string }
function M.install(opts)
	local access = opts and opts.access

	crap.pages.register("jobs", {
		section = "Tools",
		label = "Jobs",
		icon = "schedule",
		access = access,
	})

	crap.template_data.register("job_manager", page_data)

	crap.routes.register({
		path = ACTION_PREFIX .. "/trigger",
		method = "POST",
		handler = "routes.job_trigger",
		access = access,
		csrf = true,
	})

	crap.routes.register({
		path = ACTION_PREFIX .. "/cancel",
		method = "POST",
		handler = "routes.job_cancel",
		access = access,
		csrf = true,
	})
end

--- Where the action handlers send the browser back to.
M.page_url = PAGE_URL

return M
