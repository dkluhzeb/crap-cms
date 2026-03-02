--- on_init hook: runs at server startup. Logs schema info and locale config.
return function()
  ---@type { slug: string, labels: { singular?: string, plural?: string } }[]
  local collections = crap.schema.list_collections()
  ---@type string[]
  local locales = crap.locale.get_all()

  crap.log.info(
    string.format(
      "Meridian Studio: %d collections, %d locales (%s)",
      #collections,
      #locales,
      table.concat(locales, ", ")
    )
  )
end
