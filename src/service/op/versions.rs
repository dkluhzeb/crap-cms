//! The version-history operations: `list_versions` and `restore_version`.

use anyhow::anyhow;

use crate::{
    core::{Document, document::VersionSnapshot},
    service::{
        ListVersionsInput, PaginatedResult, ServiceContext, ServiceError, list_versions,
        restore_collection_version,
    },
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`ListVersions`]. Limit/offset are floored at the
/// service + db chokepoints.
#[derive(Builder)]
pub struct ListVersionsArgs {
    #[builder(required)]
    pub parent_id: String,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// List a document's version history (gated by `access.versions ?? update`;
/// draft snapshots additionally by the draft view).
pub enum ListVersions {}

impl Operation for ListVersions {
    type Args = ListVersionsArgs;
    type Output = PaginatedResult<VersionSnapshot>;

    const NAME: &'static str = "list_versions";

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let input = ListVersionsInput::builder(&args.parent_id)
            .limit(args.limit)
            .offset(args.offset)
            .build();

        list_versions(ctx, &input)
    }
}

/// Owned arguments for [`RestoreVersion`].
pub struct RestoreVersionArgs {
    pub document_id: String,
    pub version_id: String,
}

impl RestoreVersionArgs {
    #[must_use]
    pub fn new(document_id: impl Into<String>, version_id: impl Into<String>) -> Self {
        Self {
            document_id: document_id.into(),
            version_id: version_id.into(),
        }
    }
}

/// Restore a collection document to a version snapshot (update-gated; the
/// service rejects a snapshot belonging to another document).
pub enum RestoreVersion {}

impl Operation for RestoreVersion {
    type Args = RestoreVersionArgs;
    type Output = Document;

    const NAME: &'static str = "restore_version";

    const READS_VIA_CONTEXT: bool = false;

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let locale_config = ctx.locale_config.ok_or_else(|| {
            ServiceError::Internal(anyhow!("restore_version requires locale_config"))
        })?;

        restore_collection_version(ctx, &args.document_id, &args.version_id, locale_config)
    }
}
