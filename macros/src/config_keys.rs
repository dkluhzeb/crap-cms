//! `#[derive(ConfigKeys)]` — enumerate a config struct's serde keys.
//!
//! Powers the config↔docs parity test (`tests/config_doc_parity.rs`): the
//! reference tables in `docs/src/configuration/crap-toml.md` are curated by
//! hand (their Description column is real documentation, not doc-comment
//! echo), so instead of generating them we pin them — every serde key of
//! every section struct must appear as a table row, and no row may name a
//! key the struct does not have. Serialization can't provide this list:
//! `Option` fields defaulting to `None` are omitted from serialized
//! defaults but still need documenting.
//!
//! Honors `#[serde(rename = "...")]` on fields and skips
//! `#[serde(skip)]` / `#[serde(skip_deserializing)]` fields. Container
//! `rename_all` is not interpreted — the config structs only use
//! `snake_case`, which is the identity for Rust field names (guarded by a
//! unit test on the real structs).

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

pub(crate) fn run(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;

    let Data::Struct(data) = &ast.data else {
        return syn::Error::new_spanned(&ast.ident, "ConfigKeys supports only structs")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(&ast.ident, "ConfigKeys needs named fields")
            .to_compile_error()
            .into();
    };

    let mut keys = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named field");
        let serde = serde_attrs(&field.attrs);
        if serde.skipped {
            continue;
        }
        keys.push(serde.rename.unwrap_or_else(|| ident.to_string()));
    }

    let expanded = quote! {
        impl crap_cms::config::ConfigKeys for #name {
            fn config_keys() -> ::std::vec::Vec<&'static str> {
                ::std::vec![ #( #keys ),* ]
            }
        }
    };
    expanded.into()
}

/// The subset of `#[serde(...)]` field attributes the key list cares about.
struct SerdeFieldAttrs {
    skipped: bool,
    rename: Option<String>,
}

/// Scan a field's `#[serde(...)]` attributes for `skip`,
/// `skip_deserializing`, and `rename = "..."`.
fn serde_attrs(attrs: &[syn::Attribute]) -> SerdeFieldAttrs {
    let mut out = SerdeFieldAttrs {
        skipped: false,
        rename: None,
    };

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }

        // Tolerant token-level scan: serde's attribute grammar is richer
        // than we need, and unknown entries must pass through untouched.
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                out.skipped = true;
            } else if meta.path.is_ident("rename") {
                if let Ok(value) = meta.value()
                    && let Ok(lit) = value.parse::<syn::LitStr>()
                {
                    out.rename = Some(lit.value());
                }
            } else if let Ok(value) = meta.value() {
                // Consume `key = value` entries we don't care about so the
                // nested-meta parser doesn't error on them.
                let _ = value.parse::<proc_macro2::TokenStream>();
            }
            Ok(())
        });
    }

    out
}
