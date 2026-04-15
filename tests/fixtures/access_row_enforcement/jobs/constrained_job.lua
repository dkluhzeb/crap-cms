--- Job whose access hook wrongly returns a filter table — used to exercise
--- the "jobs reject Constrained" path.
local M = {}

function M.run(ctx)
    crap.log.info("constrained_job ran")
end

crap.jobs.define("constrained_job", {
    handler = "jobs.constrained_job.run",
    queue = "default",
    retries = 0,
    timeout = 10,
    labels = { singular = "Constrained Job" },
    access = "hooks.row_rules.own_rows",
})

return M
