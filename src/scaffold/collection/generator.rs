//! `make collection` — generate collection Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::cli;
use crate::scaffold::render::render;

use super::parser::{pluralize, singularize};
use super::types::{CONTAINER_TYPES, CollectionOptions, FieldStub};
use super::writer::write_field_lua;

/// Generate a collection Lua file at `<config_dir>/collections/<slug>.lua`.
///
/// Accepts pre-parsed field stubs or `None` for defaults.
pub fn make_collection(
    config_dir: &Path,
    slug: &str,
    fields: Option<&[FieldStub]>,
    opts: &CollectionOptions,
) -> Result<()> {
    crate::scaffold::validate_slug(slug)?;

    let collections_dir = config_dir.join("collections");
    fs::create_dir_all(&collections_dir).context("Failed to create collections/ directory")?;

    let file_path = collections_dir.join(format!("{}.lua", slug));

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = render_collection_lua(slug, fields, opts)?;

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));

    Ok(())
}

/// Pick the first scalar field for use_as_title / list_searchable_fields.
fn title_field<'a>(fields: &'a [FieldStub], opts: &CollectionOptions) -> Option<&'a str> {
    if opts.auth {
        return Some("email");
    }
    if opts.upload {
        return Some("filename");
    }

    fields
        .iter()
        .find(|f| {
            !CONTAINER_TYPES.contains(&f.field_type.as_str())
                && f.field_type != "blocks"
                && f.field_type != "tabs"
        })
        .map(|f| f.name.as_str())
}

/// Render the full collection Lua definition via Handlebars.
fn render_collection_lua(
    slug: &str,
    fields: Option<&[FieldStub]>,
    opts: &CollectionOptions,
) -> Result<String> {
    let singular_slug = singularize(slug);
    let label_singular = crate::scaffold::to_title_case(&singular_slug);
    let label_plural = pluralize(&label_singular);

    let default_fields;
    let fields = match fields {
        Some(f) => f,
        None if opts.upload || opts.auth => &[] as &[FieldStub],
        None => {
            default_fields = [FieldStub {
                name: "title".to_string(),
                field_type: "text".to_string(),
                required: true,
                localized: false,
                fields: vec![],
                blocks: vec![],
                tabs: vec![],
            }];
            &default_fields
        }
    };

    let mut fields_lua = String::new();
    for field in fields {
        write_field_lua(&mut fields_lua, field, 8);
    }

    render(
        "collection",
        &json!({
            "slug": slug,
            "label_singular": label_singular,
            "label_plural": label_plural,
            "timestamps": if opts.no_timestamps { "false" } else { "true" },
            "auth": opts.auth,
            "upload": opts.upload,
            "versions": opts.versions,
            "no_timestamps": opts.no_timestamps,
            "title_field": title_field(fields, opts),
            "fields_lua": fields_lua,
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::scaffold::collection::parser::parse_fields_shorthand;

    /// Helper: parse shorthand and call make_collection with the result.
    fn make_from_shorthand(
        config_dir: &Path,
        slug: &str,
        shorthand: Option<&str>,
        opts: &CollectionOptions,
    ) -> Result<()> {
        let parsed = shorthand.map(parse_fields_shorthand).transpose()?;
        make_collection(config_dir, slug, parsed.as_deref(), opts)
    }

    // ── Basic generation ────────────────────────────────────────────────

    #[test]
    fn make_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("crap.collections.define(\"posts\""));
        assert!(content.contains("singular = \"Post\""));
        assert!(content.contains("plural = \"Posts\""));
        assert!(content.contains("timestamps = true"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("required = true"));
    }

    #[test]
    fn make_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_from_shorthand(
            tmp.path(),
            "articles",
            Some("headline:text:required,body:richtext,draft:checkbox"),
            &opts,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
        assert!(content.contains("timestamps = false"));
        assert!(content.contains("name = \"headline\""));
        assert!(content.contains("use_as_title = \"headline\""));
    }

    #[test]
    fn make_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            make_collection(tmp.path(), "Bad Slug", None, &CollectionOptions::default()).is_err()
        );
    }

    #[test]
    fn refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();
        let result = make_collection(tmp.path(), "posts", None, &CollectionOptions::default());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();
        let opts = CollectionOptions {
            force: true,
            ..CollectionOptions::default()
        };
        assert!(make_collection(tmp.path(), "posts", None, &opts).is_ok());
    }

    // ── Feature flags ───────────────────────────────────────────────────

    #[test]
    fn make_auth() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("use_as_title = \"email\""));
        assert!(content.contains("list_searchable_fields = { \"email\" }"));
        assert!(!content.contains("crap.fields."));
    }

    #[test]
    fn make_auth_with_custom_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_from_shorthand(tmp.path(), "users", Some("name:text,role:select"), &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("use_as_title = \"email\""));
        assert!(content.contains("name = \"name\""));
    }

    #[test]
    fn make_upload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "media", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("use_as_title = \"filename\""));
        assert!(!content.contains("crap.fields."));
    }

    #[test]
    fn make_upload_with_custom_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_from_shorthand(
            tmp.path(),
            "media",
            Some("alt:text,caption:textarea"),
            &opts,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("upload = true"));
        assert!(content.contains("name = \"alt\""));
    }

    #[test]
    fn make_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            versions: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "posts", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("versions = true"));
    }

    #[test]
    fn make_all_flags() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            versions: true,
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("versions = true"));
        assert!(content.contains("timestamps = false"));
        assert!(!content.contains("default_sort"));
    }

    // ── Admin block ─────────────────────────────────────────────────────

    #[test]
    fn admin_block_expanded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("default_sort = \"-created_at\""));
        assert!(content.contains("list_searchable_fields = { \"title\" }"));
    }

    #[test]
    fn admin_no_default_sort_without_timestamps() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            no_timestamps: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "posts", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(!content.contains("default_sort"));
    }

    // ── Comment blocks ──────────────────────────────────────────────────

    #[test]
    fn access_block_in_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, &CollectionOptions::default()).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("-- access = {"));
        assert!(content.contains("-- indexes = {"));
    }

    #[test]
    fn upload_comment_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            upload: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "media", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/media.lua")).unwrap();
        assert!(content.contains("-- Full upload config"));
    }

    #[test]
    fn auth_comment_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("-- Full auth config"));
    }

    // ── Nested field generation ─────────────────────────────────────────

    #[test]
    fn make_with_nested_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "title:text:required,seo:group(meta_title:text,meta_desc:textarea),items:array(name:text:required,qty:number)"
        ).unwrap();
        make_collection(
            tmp.path(),
            "posts",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("name = \"seo\""));
        assert!(content.contains("name = \"meta_title\""));
        assert!(content.matches("fields = {").count() >= 3);
    }

    #[test]
    fn make_with_nested_blocks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "content:blocks(paragraph|Paragraph(body:textarea),hero|Hero(title:text,image:upload))",
        )
        .unwrap();
        make_collection(
            tmp.path(),
            "pages",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/pages.lua")).unwrap();
        assert!(content.contains("type = \"paragraph\""));
        assert!(content.contains("label = \"Hero\""));
    }

    #[test]
    fn make_with_nested_tabs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fields = parse_fields_shorthand(
            "settings:tabs(General(name:text,email:email),Advanced(api_key:text))",
        )
        .unwrap();
        make_collection(
            tmp.path(),
            "config",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/config.lua")).unwrap();
        assert!(content.contains("label = \"General\""));
        assert!(content.contains("name = \"api_key\""));
    }

    #[test]
    fn make_localized_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_from_shorthand(
            tmp.path(),
            "posts",
            Some("title:text:required:localized,body:textarea:localized"),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert_eq!(content.matches("localized = true").count(), 2);
    }

    // ── Title field selection ───────────────────────────────────────────

    #[test]
    fn all_container_fields_omit_use_as_title() {
        let fields = parse_fields_shorthand("items:array(label:text)").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "things",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/things.lua")).unwrap();
        assert!(!content.contains("use_as_title"));
        assert!(!content.contains("list_searchable_fields"));
    }

    #[test]
    fn blocks_only_omits_title() {
        let fields =
            parse_fields_shorthand("content:blocks(para|Paragraph(body:textarea))").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "pages",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/pages.lua")).unwrap();
        assert!(!content.contains("use_as_title"));
    }

    // ── Type stubs ──────────────────────────────────────────────────────

    #[test]
    fn complex_field_type_stubs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_from_shorthand(
            tmp.path(), "posts",
            Some("author:relationship,status:select,body:array,layout:blocks,meta:group,content:tabs,snippet:code,related:join,pic:upload,style:radio,section:collapsible,cols:row"),
            &CollectionOptions::default(),
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("relationship = { collection = \"other_collection\" }"));
        assert!(content.contains("options = { { label = \"Option 1\", value = \"option_1\" } }"));
        assert!(content.contains("admin = { language = \"javascript\" }"));
    }

    #[test]
    fn container_without_subfields_gets_default_stub() {
        let fields =
            parse_fields_shorthand("items:array,meta:group,layout:blocks,panels:tabs").unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(),
            "test",
            Some(&fields),
            &CollectionOptions::default(),
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/test.lua")).unwrap();
        assert!(content.contains("fields = { crap.fields.text({ name = \"item\" }) }"));
        assert!(content.contains("blocks = { { type = \"block_type\""));
    }

    // ── Combined flags ──────────────────────────────────────────────────

    #[test]
    fn make_auth_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = CollectionOptions {
            auth: true,
            versions: true,
            ..CollectionOptions::default()
        };
        make_collection(tmp.path(), "users", None, &opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/users.lua")).unwrap();
        assert!(content.contains("auth = true"));
        assert!(content.contains("versions = true"));
        assert!(content.contains("use_as_title = \"email\""));
    }
}
