-- Test job that creates a post
crap.jobs.define("test_create_post", {
    handler = "jobs.test_job.create_post",
    retries = 1,
    timeout = 30,
})

-- Test job that always fails
crap.jobs.define("test_failing_job", {
    handler = "jobs.test_job.fail",
    retries = 2,
    timeout = 30,
})

-- Test job that returns a result
crap.jobs.define("test_echo_job", {
    handler = "jobs.test_job.echo",
    timeout = 30,
})

-- Test job with cron schedule (every minute)
crap.jobs.define("test_cron_job", {
    handler = "jobs.test_job.echo",
    schedule = "* * * * *",
    timeout = 30,
    skip_if_running = true,
})

-- Test job with cron schedule and skip_if_running disabled
crap.jobs.define("test_cron_nonskip", {
    handler = "jobs.test_job.echo",
    schedule = "* * * * *",
    timeout = 30,
    skip_if_running = false,
})

local M = {}

function M.create_post(ctx)
    local title = ctx.data.title or "Job-Created Post"
    crap.collections.create("posts", {
        title = title,
        status = "published",
    })
    return { created = true }
end

function M.fail(ctx)
    error("intentional failure for testing")
end

function M.echo(ctx)
    return ctx.data
end

return M
