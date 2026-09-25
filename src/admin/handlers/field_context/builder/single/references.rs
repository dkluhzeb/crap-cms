//! Per-variant constructors for reference fields (relationship, upload,
//! join).

use crate::admin::{
    context::field::{BaseFieldData, FieldContext, JoinField, RelationshipField, UploadField},
    handlers::field_context::builder::single::entry::SingleFieldCtx,
};

pub(super) fn construct_relationship(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let (relationship_collection, has_many, polymorphic, collections) =
        if let Some(ref rc) = fc.field.relationship {
            let (poly_flag, poly_list) = if rc.is_polymorphic() {
                (
                    Some(true),
                    Some(rc.polymorphic.iter().map(ToString::to_string).collect()),
                )
            } else {
                (None, None)
            };
            (
                Some(rc.collection.to_string()),
                Some(rc.has_many),
                poly_flag,
                poly_list,
            )
        } else {
            (None, None, None, None)
        };

    let picker = fc.field.admin.picker.clone();

    FieldContext::Relationship(RelationshipField {
        base,
        relationship_collection,
        collection_singular_name: None,
        has_many,
        polymorphic,
        collections,
        picker,
        selected_items: None,
    })
}

pub(super) fn construct_upload(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let (relationship_collection, has_many) = if let Some(ref rc) = fc.field.relationship {
        let hm = if rc.has_many { Some(true) } else { None };
        (Some(rc.collection.to_string()), hm)
    } else {
        (None, None)
    };

    let picker_str = fc.field.admin.picker.as_deref().unwrap_or("drawer");
    let picker = if picker_str == "none" {
        None
    } else {
        Some(picker_str.to_string())
    };

    FieldContext::Upload(UploadField {
        base,
        relationship_collection,
        has_many,
        picker,
        selected_items: None,
        selected_filename: None,
        selected_preview_url: None,
    })
}

pub(super) fn construct_join(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    base.readonly = true;

    let (join_collection, join_on) = if let Some(ref jc) = fc.field.join {
        (Some(jc.collection.to_string()), Some(jc.on.clone()))
    } else {
        (None, None)
    };

    FieldContext::Join(JoinField {
        base,
        join_collection,
        join_on,
        join_items: None,
        join_count: None,
        join_total: None,
    })
}
