-- Cancel a pending job run from the admin Jobs page.
--
-- Mounted by `plugins/job_manager.lua` at `POST /tools/jobs/cancel`, gated by
-- the same access rule as the page and protected by the admin's double-submit
-- CSRF token: `layout/base.hbs` adds the hidden `_csrf` field to every admin
-- form, which is one of the two shapes a `csrf = true` route accepts.
--
-- Only a run that no worker has claimed can be cancelled; one already in
-- flight keeps running, which `crap.jobs.cancel_run` reports as `false`
-- rather than an error.
return crap.any.route_handler(function(ctx)
  local id = ctx.form and ctx.form.id

  if not id or id == "" then
    return { status = 400, json = { error = "id is required" } }
  end

  local cancelled = crap.jobs.cancel_run(id)

  if not cancelled then
    crap.log.info(string.format("[jobs] run %s was not cancellable", id))
  end

  return { redirect = "/admin/p/jobs" }
end)
