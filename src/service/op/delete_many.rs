//! The `delete_many` bulk operation.

use anyhow::anyhow;

use crate::{
    core::{CollectionDefinition, upload},
    db::{Filter, FilterClause, FilterOp, query::filter::normalize_filter_fields},
    service::{
        DeleteManyOptions, DeleteManyResult, ServiceContext, ServiceError, delete_many,
        validate_user_filters,
    },
};

use crate::core::Builder;

use super::Operation;
use crate::service::OpDeadline;

/// Owned arguments for [`DeleteMany`].
#[derive(Builder)]
pub struct DeleteManyArgs {
    #[builder(required)]
    pub filters: Vec<FilterClause>,
    #[builder(default = true)]
    pub run_hooks: bool,
    /// Match soft-deleted rows instead of live ones (with `trash`, or a
    /// force-hard delete that should also sweep trashed matches).
    pub include_deleted: bool,
    /// Empty-the-trash: match ONLY soft-deleted rows and permanently remove
    /// them. Adjusts the definition to hard delete (like `force_hard_delete`),
    /// appends the `_deleted_at EXISTS` restriction, and implies
    /// `include_deleted`. The exposing codecs gate this on `soft_delete`
    /// collections — on others there is no `_deleted_at` column to match.
    pub trash: bool,
    /// Permanently delete even when the collection has soft delete —
    /// expressed via [`Operation::adjust_collection_def`], like single
    /// delete.
    pub force_hard_delete: bool,
    /// `server.bulk_max_documents` cap. `0` = no limit.
    pub max_documents: i64,
    /// Publish mutation events (bulk surfaces default this to `false`).
    pub events: bool,
    /// Cooperative abort deadline for the batch (see [`OpDeadline`]).
    #[builder(default = OpDeadline::none())]
    pub deadline: OpDeadline,
}

/// Bulk-delete all documents matching a filter in one transaction.
pub enum DeleteMany {}

impl Operation for DeleteMany {
    type Args = DeleteManyArgs;
    type Output = DeleteManyResult;

    const NAME: &'static str = "delete_many";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn adjust_collection_def(
        args: &Self::Args,
        def: &CollectionDefinition,
    ) -> Option<CollectionDefinition> {
        // A trash purge hard-deletes too: with `soft_delete` cleared the
        // per-row delete permanently removes, its read stops appending
        // `_deleted_at IS NULL` (which would hide the trashed rows), and the
        // bulk gate derives `access.delete` instead of the trash gate.
        ((args.force_hard_delete || args.trash) && def.soft_delete).then(|| {
            let mut d = def.clone();
            d.make_hard_delete();
            d
        })
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let locale_config = ctx
            .locale_config
            .ok_or_else(|| ServiceError::Internal(anyhow!("delete_many requires locale_config")))?;

        // Same wire-filter hygiene as `Find::run`, BEFORE the system trash
        // filter is appended (user filters must not touch system columns; the
        // body-owned `_deleted_at` restriction legitimately does).
        let mut filters = args.filters;
        normalize_filter_fields(&mut filters, ctx.fields()?);
        validate_user_filters(&filters).map_err(|e| ServiceError::HookError(e.to_string()))?;

        // Trash purge: restrict the match-set to physically-trashed rows.
        // Without this the include_deleted find would also match live rows.
        // Lives in the shared body (not the codecs) so admin empty-trash and
        // Lua `trash = true` cannot drift.
        if args.trash {
            filters.push(FilterClause::Single(Filter {
                field: "_deleted_at".to_string(),
                op: FilterOp::Exists,
            }));
        }

        let opts = DeleteManyOptions {
            run_hooks: args.run_hooks,
            include_deleted: args.include_deleted || args.trash,
            max_documents: args.max_documents,
            deadline: args.deadline,
        };

        let result = delete_many(ctx, &filters, locale_config, &opts)?;

        // Pool mode committed internally — clean the deleted uploads' files
        // post-commit HERE so no codec can forget it. Conn mode (Lua, inside
        // the hook transaction) leaves cleanup to the surface: deleting files
        // before the caller's commit would break the files-after-commit
        // invariant.
        if ctx.pool.is_some()
            && let Some(storage) = &ctx.storage
        {
            for fields in &result.upload_fields_to_clean {
                upload::delete_upload_files(storage.as_ref(), fields);
            }
        }

        Ok(result)
    }
}
