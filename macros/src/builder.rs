//! `#[derive(Builder)]` — generates the house builder convention.
//!
//! The codebase's hand-written builders all share one shape (see CLAUDE.md):
//! `Type::builder(<required fields, positional>)` returning a `TypeBuilder`
//! with one `#[must_use]` chained setter per optional field and an infallible
//! `build()`. This derive generates exactly that shape, so adopting it is a
//! pure deletion of the hand-written halves — zero call-site changes — and a
//! forgotten required field stays a *compile* error (it is a missing
//! `builder()` argument, never a runtime panic).
//!
//! ## Field rules
//!
//! - `#[builder(required)]` — becomes a positional `builder()` parameter, in
//!   declaration order. A required `String` parameter is taken as
//!   `impl Into<String>` (so `builder("x")` works); everything else by value.
//! - Optional fields get a chained setter taking the field's type as-is —
//!   `Option<T>` setters take `Option<T>` (the caller's already-optional
//!   value flows through without `if let`).
//! - Optional-field defaults: `bool → false`, `Option<_> → None`, the
//!   integer/float primitives → `0`/`0.0`, `Vec<_> → Vec::new()`. Any other
//!   type (or any different value) must say `#[builder(default = <expr>)]`
//!   explicitly — no clever inference for values that matter.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, Field, Fields, GenericArgument, PathArguments, Type,
    parse_macro_input, spanned::Spanned,
};

/// One parsed field of the target struct.
struct BuilderField<'a> {
    field: &'a Field,
    required: bool,
    default: Option<Expr>,
}

fn parse_fields(input: &DeriveInput) -> syn::Result<Vec<BuilderField<'_>>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "#[derive(Builder)] only supports structs",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new(
            input.span(),
            "#[derive(Builder)] requires named fields",
        ));
    };

    let mut out = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let mut required = false;
        let mut default: Option<Expr> = None;

        for attr in &field.attrs {
            if !attr.path().is_ident("builder") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("required") {
                    required = true;
                    return Ok(());
                }
                if meta.path.is_ident("default") {
                    default = Some(meta.value()?.parse()?);
                    return Ok(());
                }
                Err(meta.error("expected `required` or `default = <expr>`"))
            })?;
        }

        if required && default.is_some() {
            return Err(syn::Error::new(
                field.span(),
                "a field cannot be both `required` and have a `default`",
            ));
        }

        out.push(BuilderField {
            field,
            required,
            default,
        });
    }
    Ok(out)
}

/// The inferred default expression for an optional field without an explicit
/// `#[builder(default = ...)]`, or an error naming the field.
fn inferred_default(field: &Field) -> syn::Result<TokenStream> {
    let ty = &field.ty;
    let Type::Path(path) = ty else {
        return Err(needs_default(field));
    };
    let Some(seg) = path.path.segments.last() else {
        return Err(needs_default(field));
    };

    let ident = seg.ident.to_string();
    let ts = match ident.as_str() {
        "bool" => quote!(false),
        "Option" => quote!(None),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => quote!(0),
        "f32" | "f64" => quote!(0.0),
        "Vec" => quote!(Vec::new()),
        _ => return Err(needs_default(field)),
    };
    Ok(ts)
}

fn needs_default(field: &Field) -> syn::Error {
    syn::Error::new(
        field.span(),
        "field needs #[builder(required)] or #[builder(default = <expr>)] \
         (defaults are inferred only for bool, Option, integers, floats, and Vec)",
    )
}

/// True when the type is exactly `String` (required-parameter `impl Into`
/// coercion).
fn is_string(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| {
        s.ident == "String" && matches!(s.arguments, PathArguments::None)
    }))
}

/// True for `Option<...>` — used only to keep the setter type exactly the
/// field type (no unwrapping magic).
#[allow(dead_code)]
fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

pub fn derive_builder(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let fields = match parse_fields(&input) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };

    let vis = &input.vis;
    let name = &input.ident;
    let builder_name = format_ident!("{name}Builder");
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // builder() parameters + seeding for required fields.
    let mut params = Vec::new();
    let mut seeds = Vec::new();
    // Builder struct fields + optional setters + build() assignments.
    let mut builder_fields = Vec::new();
    let mut setters = Vec::new();
    let mut build_assigns = Vec::new();

    for bf in &fields {
        let ident = bf.field.ident.as_ref().expect("named field");
        let ty = &bf.field.ty;

        builder_fields.push(quote!(#ident: #ty));
        build_assigns.push(quote!(#ident: self.#ident));

        if bf.required {
            if is_string(ty) {
                params.push(quote!(#ident: impl Into<String>));
                seeds.push(quote!(#ident: #ident.into()));
            } else {
                params.push(quote!(#ident: #ty));
                seeds.push(quote!(#ident: #ident));
            }
            continue;
        }

        let default = match &bf.default {
            Some(expr) => quote!(#expr),
            None => match inferred_default(bf.field) {
                Ok(ts) => ts,
                Err(e) => return e.to_compile_error().into(),
            },
        };
        seeds.push(quote!(#ident: #default));

        let doc = format!("Set `{ident}` (see the field on the built type).");
        setters.push(quote! {
            #[doc = #doc]
            #[must_use]
            pub fn #ident(mut self, #ident: #ty) -> Self {
                self.#ident = #ident;
                self
            }
        });
    }

    let builder_doc = format!("Builder for [`{name}`], via `{name}::builder(..)`.");
    let builder_fn_doc = format!(
        "Start building a [`{name}`]: required fields positionally, optional \
         fields via the chained setters, then `build()`."
    );

    let expanded = quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            #[doc = #builder_fn_doc]
            #[must_use]
            #vis fn builder(#(#params),*) -> #builder_name #ty_generics {
                #builder_name {
                    #(#seeds),*
                }
            }
        }

        #[doc = #builder_doc]
        #vis struct #builder_name #ty_generics #where_clause {
            #(#builder_fields),*
        }

        impl #impl_generics #builder_name #ty_generics #where_clause {
            #(#setters)*

            /// Finalize the built value. Infallible — required fields were
            /// `builder()` parameters, so nothing can be missing.
            #[must_use]
            pub fn build(self) -> #name #ty_generics {
                #name {
                    #(#build_assigns),*
                }
            }
        }
    };

    expanded.into()
}
