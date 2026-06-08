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

-- Atomic multi-step job: both creates wrapped in crap.transaction.
crap.jobs.define("test_tx_two_creates", {
    handler = "jobs.test_job.tx_two_creates",
    timeout = 30,
})

-- Atomic rollback job: first create succeeds inside the tx, then we
-- raise an error — the framework must roll back so NEITHER create is
-- visible after the job completes.
crap.jobs.define("test_tx_rollback_mid", {
    handler = "jobs.test_job.tx_rollback_mid",
    timeout = 30,
})

-- Pool-mode demo: two SEPARATE crap.transaction blocks. If the second
-- errors, the first must still be committed (each block is its own
-- IMMEDIATE tx).
crap.jobs.define("test_tx_separate_blocks", {
    handler = "jobs.test_job.tx_separate_blocks",
    timeout = 30,
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

function M.tx_two_creates(ctx)
    crap.transaction(function()
        crap.collections.create("posts", {
            title = "tx-doc-1",
            status = "published",
        })
        crap.collections.create("posts", {
            title = "tx-doc-2",
            status = "published",
        })
    end)
    return { ok = true }
end

function M.tx_rollback_mid(ctx)
    -- pcall so the job itself completes (status=completed); the
    -- transaction inside still rolled back.
    local ok, err = pcall(function()
        crap.transaction(function()
            crap.collections.create("posts", {
                title = "should-not-exist",
                status = "published",
            })
            error("force rollback")
        end)
    end)
    return { ok = ok, err = tostring(err) }
end

function M.tx_separate_blocks(ctx)
    -- First block: commits standalone.
    crap.transaction(function()
        crap.collections.create("posts", {
            title = "block-1-doc",
            status = "published",
        })
    end)

    -- Second block: errors. The first block's write is already
    -- committed and must remain visible.
    local ok, _ = pcall(function()
        crap.transaction(function()
            crap.collections.create("posts", {
                title = "block-2-doc",
                status = "published",
            })
            error("rollback only block 2")
        end)
    end)
    return { second_ok = ok }
end

-- Echo the run metadata so a test can assert the handler context's `job`
-- table carries id / queue / priority / scheduled_by / queued_at / unique_key.
function M.job_meta(ctx)
    return {
        id = ctx.job.id,
        slug = ctx.job.slug,
        queue = ctx.job.queue,
        priority = ctx.job.priority,
        scheduled_by = ctx.job.scheduled_by,
        queued_at = ctx.job.queued_at,
        unique_key = ctx.job.unique_key,
    }
end

return M
