--- SEO plugin: injects meta title, description, and noindex fields into collections.
local M = {}

local seo_fields = crap.fields.group({
  name = "seo",
  admin = {
    label = "SEO",
    position = "sidebar",
  },
  fields = {
    crap.fields.text({
      name = "meta_title",
      admin = { placeholder = "Override page title for search engines" },
    }),
    crap.fields.textarea({
      name = "meta_description",
      admin = { rows = 2, placeholder = "155 characters recommended" },
    }),
    crap.fields.checkbox({
      name = "no_index",
      default_value = false,
      admin = { description = "Prevent search engines from indexing" },
    }),
  },
})

---@param opts? { exclude: string[] }
function M.install(opts)
  local exclude = opts and opts.exclude
  local all = crap.collections.config.list()

  for slug, def in pairs(all) do
    if not def then
      goto continue
    end

    -- Skip upload and auth collections
    if def.upload or def.auth then
      goto continue
    end

    -- Skip excluded collections
    if exclude then
      local skip = false
      for _, ex in ipairs(exclude) do
        if ex == slug then
          skip = true
          break
        end
      end
      if skip then
        goto continue
      end
    end

    -- Check if SEO fields already exist
    local has_seo = false
    for _, field in ipairs(def.fields) do
      if field.name == "seo" then
        has_seo = true
        break
      end
    end

    if not has_seo then
      def.fields[#def.fields + 1] = seo_fields
      crap.collections.define(slug, def)
      crap.log.info("seo: added SEO fields to " .. slug)
    end

    ::continue::
  end
end

return M
