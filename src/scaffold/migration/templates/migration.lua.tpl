local M = {}

function M.up()
    -- Example: update all documents in a collection
    -- local docs = crap.collections.find("posts", { where = { status = { equals = "draft" } } })
    -- for _, doc in ipairs(docs) do
    --     crap.collections.update("posts", doc.id, { status = "published" })
    -- end
end

function M.down()
    -- Reverse the migration (best-effort, optional)
end

return M
