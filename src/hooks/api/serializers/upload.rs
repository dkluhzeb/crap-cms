//! Lua table serializer for collection upload configuration.

use mlua::{Lua, Table};

use crate::core::{CollectionDefinition, upload::ImageFit};

/// Serialize the upload section of a CollectionDefinition into the Lua table.
pub(super) fn collection_upload_to_lua(
    lua: &Lua,
    tbl: &Table,
    def: &CollectionDefinition,
) -> mlua::Result<()> {
    if let Some(ref upload) = def.upload
        && upload.enabled
    {
        if upload.mime_types.is_empty()
            && upload.max_file_size.is_none()
            && upload.image_sizes.is_empty()
            && upload.admin_thumbnail.is_none()
            && upload.format_options.webp.is_none()
            && upload.format_options.avif.is_none()
        {
            tbl.set("upload", true)?;
        } else {
            let u = lua.create_table()?;

            if !upload.mime_types.is_empty() {
                let mt = lua.create_table()?;

                for (i, m) in upload.mime_types.iter().enumerate() {
                    mt.set(i + 1, m.as_str())?;
                }

                u.set("mime_types", mt)?;
            }
            if let Some(max) = upload.max_file_size {
                u.set("max_file_size", max)?;
            }
            if !upload.image_sizes.is_empty() {
                let sizes = lua.create_table()?;

                for (i, s) in upload.image_sizes.iter().enumerate() {
                    let st = lua.create_table()?;
                    st.set("name", s.name.as_str())?;
                    st.set("width", s.width)?;
                    st.set("height", s.height)?;
                    let fit_str = match s.fit {
                        ImageFit::Cover => "cover",
                        ImageFit::Contain => "contain",
                        ImageFit::Inside => "inside",
                        ImageFit::Fill => "fill",
                    };
                    st.set("fit", fit_str)?;
                    sizes.set(i + 1, st)?;
                }

                u.set("image_sizes", sizes)?;
            }
            if let Some(ref thumb) = upload.admin_thumbnail {
                u.set("admin_thumbnail", thumb.as_str())?;
            }
            if upload.format_options.webp.is_some() || upload.format_options.avif.is_some() {
                let fo = lua.create_table()?;

                if let Some(ref webp) = upload.format_options.webp {
                    let w = lua.create_table()?;
                    w.set("quality", webp.quality)?;
                    fo.set("webp", w)?;
                }

                if let Some(ref avif) = upload.format_options.avif {
                    let a = lua.create_table()?;
                    a.set("quality", avif.quality)?;
                    fo.set("avif", a)?;
                }

                u.set("format_options", fo)?;
            }
            tbl.set("upload", u)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::collection::collection_config_to_lua;
    use crate::core::{
        CollectionDefinition,
        upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit, ImageSizeBuilder},
    };
    use mlua::{self, Value};

    #[test]
    fn test_collection_config_to_lua_with_upload() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("media");
        def.timestamps = true;
        {
            let mut upload = CollectionUpload::new();
            upload.mime_types = vec!["image/png".to_string()];
            upload.max_file_size = Some(1000000);
            upload.image_sizes = vec![
                ImageSizeBuilder::new("thumb")
                    .width(200)
                    .height(200)
                    .fit(ImageFit::Cover)
                    .build(),
            ];
            upload.admin_thumbnail = Some("thumb".to_string());
            upload.format_options = FormatOptions {
                webp: Some(FormatQuality::new(80, false)),
                avif: None,
            };
            def.upload = Some(upload);
        }
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let upload: mlua::Table = tbl.get("upload").unwrap();
        let mt: mlua::Table = upload.get("mime_types").unwrap();
        let m1: String = mt.get(1).unwrap();
        assert_eq!(m1, "image/png");
        assert_eq!(upload.get::<u64>("max_file_size").unwrap(), 1000000);
        let sizes: mlua::Table = upload.get("image_sizes").unwrap();
        let s1: mlua::Table = sizes.get(1).unwrap();
        assert_eq!(s1.get::<String>("name").unwrap(), "thumb");
        assert_eq!(s1.get::<String>("fit").unwrap(), "cover");
        let fo: mlua::Table = upload.get("format_options").unwrap();
        let webp: mlua::Table = fo.get("webp").unwrap();
        assert_eq!(webp.get::<u8>("quality").unwrap(), 80);
    }

    #[test]
    fn test_collection_config_to_lua_upload_simple_true() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("media");
        def.timestamps = false;
        def.upload = Some(CollectionUpload::new());
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let upload_val: bool = tbl.get("upload").unwrap();
        assert!(upload_val, "Simple upload should serialize as true");
    }

    #[test]
    fn test_collection_config_to_lua_upload_avif_only() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("media");
        def.timestamps = false;
        {
            let mut upload = CollectionUpload::new();
            upload.format_options = FormatOptions {
                webp: None,
                avif: Some(FormatQuality::new(60, false)),
            };
            def.upload = Some(upload);
        }
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let upload: mlua::Table = tbl.get("upload").unwrap();
        let fo: mlua::Table = upload.get("format_options").unwrap();
        let avif: mlua::Table = fo.get("avif").unwrap();
        assert_eq!(avif.get::<u8>("quality").unwrap(), 60);
        let webp_val: Value = fo.get("webp").unwrap();
        assert!(matches!(webp_val, Value::Nil));
    }

    #[test]
    fn test_collection_config_to_lua_image_fit_variants() {
        let lua = mlua::Lua::new();

        let fits = [
            (ImageFit::Contain, "contain"),
            (ImageFit::Inside, "inside"),
            (ImageFit::Fill, "fill"),
        ];

        for (fit, expected_str) in fits {
            let mut def = CollectionDefinition::new("media");
            def.timestamps = false;
            let mut upload = CollectionUpload::new();
            upload.image_sizes = vec![
                ImageSizeBuilder::new("thumb")
                    .width(100)
                    .height(100)
                    .fit(fit)
                    .build(),
            ];
            def.upload = Some(upload);
            let tbl = collection_config_to_lua(&lua, &def).unwrap();
            let upload: mlua::Table = tbl.get("upload").unwrap();
            let sizes: mlua::Table = upload.get("image_sizes").unwrap();
            let s1: mlua::Table = sizes.get(1).unwrap();
            assert_eq!(
                s1.get::<String>("fit").unwrap(),
                expected_str,
                "Expected fit='{}' for {:?}",
                expected_str,
                expected_str
            );
        }
    }
}
