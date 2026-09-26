//! What an edit form renders for its viewer: the declared fields minus every
//! field the viewer may not read, at every depth.
//!
//! The service read strips the values of those fields; the form must not
//! render an input for them either — an input for a value the viewer cannot
//! see renders empty, and saving it would submit that blank. The write keeps
//! such values as stored whatever arrives (see the update strip), so the
//! form's part is only to not offer an input that could never work.
//!
//! A document-level field (at the top level or in a group) is dropped from the
//! form's field list. An array/blocks row value is judged row by row — a
//! data-aware rule can hide it in one row and show it in the next — so the
//! rows keep their full schema and each rendered row is pruned by what its
//! viewer may read there ([`RowReadDenials`](super::row_denials::RowReadDenials)).
//!
//! The same field list is what the form's submission is parsed against: the
//! parse reads a checkbox the form rendered but that submitted nothing as
//! unchecked, and it must not read one the form never rendered that way. In a
//! row, a checkbox the form did not render there is read as unchecked and the
//! write keeps the stored value anyway: it judges that row exactly as the form
//! did, and a new row against an empty one, as the form's template is.

use std::{borrow::Cow, sync::Arc};

use axum::Extension;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{compute_denied_read_fields, editor_read_ctx},
    },
    core::{
        AuthUser, CollectionDefinition, DocumentFields, FieldChildren, FieldDefinition,
        FieldDenial, GlobalDefinition, field_children, prefixed_name,
    },
    hooks::lifecycle::access::has_any_field_access,
    service::op::{self, FindById, FindByIdArgs, GetGlobal, GetGlobalArgs, Principal, TargetRef},
};

use super::row_denials::FormReadDenials;

/// The fields an edit form renders for a viewer denied `denied`: every
/// document-level field the denial list names is dropped, whether it sits at
/// the top level, inside a group or inside a layout wrapper. Array/blocks rows
/// keep their full schema: their values are judged row by row.
///
/// Borrows `fields` unchanged when nothing is denied (the common case).
#[must_use]
pub fn readable_form_fields<'a>(
    fields: &'a [FieldDefinition],
    denied: &[FieldDenial],
) -> Cow<'a, [FieldDefinition]> {
    if !denied.iter().any(|d| matches!(d, FieldDenial::Flat(_))) {
        return Cow::Borrowed(fields);
    }

    Cow::Owned(prune_flat(fields, "", denied))
}

/// Prune one document-level list — the recursion of `collect_denials_flat`,
/// so the path each field is matched by is exactly the path a denial names.
fn prune_flat(
    fields: &[FieldDefinition],
    prefix: &str,
    denied: &[FieldDenial],
) -> Vec<FieldDefinition> {
    fields
        .iter()
        .filter_map(|field| {
            let full_name = prefixed_name(prefix, &field.name);

            if denied.contains(&FieldDenial::Flat(full_name.clone())) {
                return None;
            }

            let mut field = field.clone();
            prune_flat_children(&mut field, prefix, &full_name, denied);

            Some(field)
        })
        .collect()
}

/// Prune the document-level children of a field kept by [`prune_flat`].
fn prune_flat_children(
    field: &mut FieldDefinition,
    prefix: &str,
    full_name: &str,
    denied: &[FieldDenial],
) {
    match field_children(field) {
        FieldChildren::Group(_) => field.fields = prune_flat(&field.fields, full_name, denied),
        FieldChildren::Wrapper(_) => field.fields = prune_flat(&field.fields, prefix, denied),
        FieldChildren::Tabs(_) => {
            for tab in &mut field.tabs {
                tab.fields = prune_flat(&tab.fields, prefix, denied);
            }
        }
        FieldChildren::Array(_) | FieldChildren::Blocks(_) | FieldChildren::Leaf => {}
    }
}

/// The pre-resolved admin principal the edit forms read as.
fn principal(auth_user: Option<&Extension<AuthUser>>) -> Principal {
    Principal::Resolved {
        user: auth_user.map(|Extension(au)| au.user_doc.clone()),
        ui_locale: auth_user.map(|Extension(au)| au.ui_locale.clone()),
    }
}

/// What `auth_user` may not read on document `id`, for a form rendered
/// without the document in hand (the edit form's error re-render, the
/// submission parse). Reads the document as the edit form does and judges it
/// the same way. Fails closed: when the document cannot be read, every
/// read-gated field is withheld.
pub async fn collection_read_denials(
    state: &AdminState,
    def: &CollectionDefinition,
    id: &str,
    auth_user: Option<&Extension<AuthUser>>,
    locale: Option<&str>,
) -> FormReadDenials {
    if !has_any_field_access(&def.fields, |f| f.access.read.as_ref()) {
        return FormReadDenials::default();
    }

    let args = FindByIdArgs::builder(id)
        .locale_ctx(editor_read_ctx(state, locale))
        .use_draft(true)
        .build();

    let read = op::run_blocking::<FindById>(
        Arc::clone(&state.infra),
        principal(auth_user),
        TargetRef::collection(def.slug.to_string()),
        args,
    )
    .await;

    let Ok(Some(document)) = read else {
        return FormReadDenials::deny_all(&def.fields, &DocumentFields::new());
    };

    compute_denied_read_fields(state, auth_user, &def.fields, &def.slug, &document.fields)
        .unwrap_or_else(|_| FormReadDenials::deny_all(&def.fields, &document.fields))
}

/// The global twin of [`collection_read_denials`].
pub async fn global_read_denials(
    state: &AdminState,
    def: &GlobalDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    locale: Option<&str>,
) -> FormReadDenials {
    if !has_any_field_access(&def.fields, |f| f.access.read.as_ref()) {
        return FormReadDenials::default();
    }

    let args = GetGlobalArgs::builder()
        .locale_ctx(editor_read_ctx(state, locale))
        .include_drafts(true)
        .build();

    let read = op::run_blocking::<GetGlobal>(
        Arc::clone(&state.infra),
        principal(auth_user),
        TargetRef::global(def.slug.to_string()),
        args,
    )
    .await;

    let Ok(document) = read else {
        return FormReadDenials::deny_all(&def.fields, &DocumentFields::new());
    };

    compute_denied_read_fields(state, auth_user, &def.fields, &def.slug, &document.fields)
        .unwrap_or_else(|_| FormReadDenials::deny_all(&def.fields, &document.fields))
}

/// The fields `auth_user`'s edit form of document `id` rendered, judged as
/// that form judged them — the fields a submission of the form is parsed
/// against, so a document-level input it never rendered is absent from the
/// write, never an unchecked box. Rows keep their full schema (see the module
/// docs).
pub async fn collection_form_fields<'a>(
    state: &AdminState,
    def: &'a CollectionDefinition,
    id: &str,
    auth_user: Option<&Extension<AuthUser>>,
    locale: Option<&str>,
) -> Cow<'a, [FieldDefinition]> {
    let denied = collection_read_denials(state, def, id, auth_user, locale).await;

    readable_form_fields(&def.fields, &denied.flat)
}

/// The global twin of [`collection_form_fields`].
pub async fn global_form_fields<'a>(
    state: &AdminState,
    def: &'a GlobalDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    locale: Option<&str>,
) -> Cow<'a, [FieldDefinition]> {
    let denied = global_read_denials(state, def, auth_user, locale).await;

    readable_form_fields(&def.fields, &denied.flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{BlockDefinition, FieldAccess, FieldType, HookRef};

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn gated(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(FieldAccess {
                read: Some(HookRef::from("hooks.deny")),
                ..Default::default()
            })
            .build()
    }

    /// Every field name at every depth, in declaration order.
    fn names(fields: &[FieldDefinition]) -> Vec<String> {
        let mut out = Vec::new();

        for field in fields {
            out.push(field.name.clone());
            out.extend(names(&field.fields));

            for tab in &field.tabs {
                out.extend(names(&tab.fields));
            }
            for block in &field.blocks {
                out.extend(names(&block.fields));
            }
        }

        out
    }

    fn schema() -> Vec<FieldDefinition> {
        vec![
            text("title"),
            gated("secret"),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![gated("side")])
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text("label"), gated("note")])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    text("caption"),
                    gated("hidden"),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![gated("deep")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "quote",
                    vec![text("text"), gated("source")],
                )])
                .build(),
        ]
    }

    /// Regression: the edit form dropped only TOP-LEVEL denied fields, so a
    /// denied group sub-field and a denied field inside a row wrapper
    /// rendered as empty inputs. Array/blocks rows keep their full schema:
    /// their values are judged row by row, and each rendered row is pruned by
    /// what its viewer may read there.
    #[test]
    fn a_denied_document_level_field_is_dropped_at_every_depth() {
        let fields = schema();
        let denied = FormReadDenials::deny_all(&fields, &DocumentFields::new());

        let kept = readable_form_fields(&fields, &denied.flat);

        assert_eq!(
            names(&kept),
            vec![
                "title", "layout", "seo", "label", "items", "caption", "hidden", "meta", "deep",
                "body", "text", "source"
            ]
        );
    }

    #[test]
    fn nothing_denied_borrows_the_declared_fields() {
        let fields = schema();

        assert!(matches!(
            readable_form_fields(&fields, &[]),
            Cow::Borrowed(_)
        ));
    }
}
