-- Queue a job from the admin Jobs page.
--
-- Mounted by `plugins/job_manager.lua` at `POST /tools/jobs/trigger`, gated by
-- the same access rule as the page and protected by the admin's double-submit
-- CSRF token: `layout/base.hbs` adds the hidden `_csrf` field to every admin
-- form, which is one of the two shapes a `csrf = true` route accepts.
--
-- Triggering carries no payload on purpose: a free-text JSON box would be a
-- new untrusted input surface. A job that needs input stays a CLI or Lua call.
return crap.any.route_handler(function(ctx)
  local slug = ctx.form and ctx.form.slug

  if not slug or slug == "" then
    return { status = 400, json = { error = "slug is required" } }
  end

  -- `crap.jobs.queue` applies the job's own access rule with operation
  -- "trigger", so a caller who may see a job in the list is still refused
  -- here if the rule only grants reads.
  local ok, result = pcall(crap.jobs.queue, slug, {}, { priority = 0 })

  if not ok then
    crap.log.warn(string.format("[jobs] trigger %s refused: %s", slug, tostring(result)))

    return { status = 403, json = { error = "Could not queue that job" } }
  end

  crap.log.info(string.format("[jobs] queued %s as run %s", slug, tostring(result)))

  return { redirect = "/admin/p/jobs" }
end)
