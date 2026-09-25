//! Keeping a written document's full-text search entry current.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    db::{
        DbConnection,
        query::fts::{FtsIndex, fts_upsert},
    },
    service::ServiceContext,
};

/// Re-index document `id` of the context's collection for full-text search,
/// resolving rich text custom nodes' `searchable_attrs` through the write's
/// registry ([`ServiceContext::schema_registry`]). No-op on a backend without
/// full-text search.
///
/// # Errors
///
/// Returns an error when the context carries no collection definition, or a
/// backend error from the index write.
pub(crate) fn sync_search_index(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_config: &LocaleConfig,
) -> Result<()> {
    if !conn.supports_fts() {
        return Ok(());
    }

    let index = FtsIndex::builder(ctx.slug, ctx.collection_def()?, locale_config)
        .registry(ctx.schema_registry())
        .build();

    fts_upsert(conn, &index, id)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, DocumentFields, FieldAdmin, FieldDefinition, FieldType, Registry,
            RichtextNodeDef,
        },
        db::{DbPool, DbValue, migrate, pool, query},
        hooks::HookRunner,
        service::{LuaWriteHooks, WriteInput, create_document, update_document},
    };

    struct Harness {
        tmp: tempfile::TempDir,
        pool: DbPool,
        registry: Arc<Registry>,
        def: CollectionDefinition,
    }

    /// A collection whose JSON rich text `body` enables a `cta` node with a
    /// searchable `label` attr.
    fn setup() -> Harness {
        let mut def = CollectionDefinition::new("pages");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .richtext_format("json")
                        .nodes(vec!["cta".to_string()])
                        .build(),
                )
                .build(),
        ];

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(def.clone());
            reg.register_richtext_node(
                RichtextNodeDef::builder("cta", "CTA")
                    .attrs(vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                    ])
                    .searchable_attrs(vec!["label".to_string()])
                    .build(),
            );
        }
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&pool, &registry, &LocaleConfig::default()).expect("sync");

        Harness {
            tmp,
            pool,
            registry,
            def,
        }
    }

    fn body_with_cta(label: &str) -> DocumentFields {
        let body = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "Intro" }] },
            { "type": "cta", "attrs": { "label": label } },
        ]});

        [
            ("title".to_string(), json!("Page")),
            ("body".to_string(), json!(body.to_string())),
        ]
        .into_iter()
        .collect()
    }

    /// Which ids the search index matches `term` for.
    fn indexed_ids(h: &Harness, term: &str) -> Vec<String> {
        let conn = h.pool.get().unwrap();

        conn.query_all(
            "SELECT id FROM _fts_pages WHERE _fts_pages MATCH ?1",
            &[DbValue::Text(format!("\"{term}\""))],
        )
        .unwrap()
        .iter()
        .map(|r| r.get_string("id").unwrap())
        .collect()
    }

    /// Regression: the per-write index sync passed no registry, so a custom
    /// node's `searchable_attrs` were never indexed on create or update — and
    /// the startup rebuild had no registry either.
    #[test]
    fn searchable_node_attrs_are_indexed_on_create_update_and_rebuild() {
        let h = setup();
        let runner = HookRunner::builder()
            .config_dir(h.tmp.path())
            .registry(Arc::clone(&h.registry))
            .config(&CrapConfig::test_default())
            .build()
            .expect("runner");
        let ctx = ServiceContext::collection("pages", &h.def)
            .pool(&h.pool)
            .runner(&runner)
            .build();

        let (created, _) =
            create_document(&ctx, WriteInput::builder(body_with_cta("Zeppelin")).build())
                .expect("create");
        let id = created.id.to_string();
        assert_eq!(indexed_ids(&h, "Zeppelin"), vec![id.clone()], "create");

        update_document(
            &ctx,
            &id,
            WriteInput::builder(body_with_cta("Dirigible")).build(),
        )
        .expect("update");
        assert_eq!(indexed_ids(&h, "Dirigible"), vec![id.clone()], "update");
        assert!(indexed_ids(&h, "Zeppelin").is_empty(), "old attr text gone");

        migrate::sync_all(&h.pool, &h.registry, &LocaleConfig::default()).expect("resync");
        assert_eq!(indexed_ids(&h, "Dirigible"), vec![id], "startup rebuild");
    }

    /// Lua CRUD attaches no registry to its service context; the index
    /// resolves custom nodes through the write hooks' registry instead.
    #[test]
    fn lua_crud_context_resolves_nodes_through_its_write_hooks() {
        let h = setup();
        let lua = mlua::Lua::new();
        let hooks = LuaWriteHooks::builder(&lua, &h.registry).build();
        let conn = h.pool.get().unwrap();
        let ctx = ServiceContext::collection("pages", &h.def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();
        assert!(ctx.registry.is_none());

        let doc = query::create(&conn, "pages", &h.def, &body_with_cta("Blimp"), None).unwrap();
        sync_search_index(&ctx, &conn, &doc.id, &LocaleConfig::default()).unwrap();

        assert_eq!(indexed_ids(&h, "Blimp"), vec![doc.id.to_string()]);
    }
}
