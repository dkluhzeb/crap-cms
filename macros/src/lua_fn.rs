//! `#[lua_fn(...)]` — attribute macro that turns a Rust function into a
//! Lua API binding.
//!
//! Generates:
//! 1. The function itself, unchanged (per-arg `#[lua(...)]` attrs
//!    stripped).
//! 2. A `const FN_NAME_SPEC: LuaFnSpec` with typegen metadata.
//! 3. A `fn FN_NAME_register(lua, state)` wrapper that builds the
//!    `mlua::Function` for registration via the `lua_table!` macro.
//!
//! The first parameter of the function determines stateful-ness:
//! - `&Lua` / `&mlua::Lua` → stateless. State arg in the wrapper is
//!   `Arc<()>` and discarded.
//! - Otherwise → stateful. The first param's referent type is the
//!   state type, threaded through as `Arc<State>` in the wrapper.
//!
//! All remaining parameters become Lua-facing `LuaParam` entries.

use darling::{FromMeta, ast::NestedMeta};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, FnArg, ItemFn, Pat, PatType, ReturnType, Type, parse_macro_input, spanned::Spanned,
};

use crate::shared::{extract_docs, strip_ref, unwrap_path};

/// Attribute parser for
/// `#[lua_fn(path = "...", returns = "...", returns_doc = "...")]`.
///
/// `path` is the dotted Lua path the function is registered under.
/// `returns` is an optional string override for the Lua return type —
/// use when the Rust return type doesn't auto-map (e.g. `Value` that's
/// polymorphic between `Nil` and `Table`).
/// `returns_doc` is an optional trailing prose for the `--- @return`
/// line (e.g. `"Random nanoid string."` produces
/// `--- @return string  Random nanoid string.`).
#[derive(FromMeta)]
struct LuaFnAttr {
    path: String,
    #[darling(default)]
    returns: Option<String>,
    #[darling(default)]
    returns_doc: Option<String>,
    /// When `true`, the generated `_register` wrapper routes the user
    /// fn call through `with_lua_db`, which installs a per-op
    /// `TxContext` when running in pool-mode (job handler). User code
    /// keeps calling `get_tx_conn(lua)?` exactly as before — the
    /// wrapper just makes the same call work transparently for both
    /// hook-mode (shared tx) and job-mode (per-op IMMEDIATE tx).
    ///
    /// Opt-in: only DB-touching `#[lua_fn]` declarations should set
    /// this. Plain utility fns (`crap.json.encode`, `crap.log.info`)
    /// don't need it and would error in untyped Lua contexts where
    /// no transaction is appropriate.
    #[darling(default)]
    auto_tx: bool,
}

/// Per-parameter attribute: `#[lua(ty = "...")]` on a function
/// argument. Reuses the same `lua` attribute namespace as
/// `LuaAnnotation` fields.
#[derive(FromMeta, Default)]
struct LuaParamAttr {
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    doc: Option<String>,
}

pub(crate) fn run(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_meta = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(m) => m,
        Err(e) => return darling::Error::from(e).write_errors().into(),
    };
    let attr_args = match LuaFnAttr::from_list(&attr_meta) {
        Ok(a) => a,
        Err(e) => return e.write_errors().into(),
    };
    let mut item_fn = parse_macro_input!(item as ItemFn);

    match expand(&attr_args, &mut item_fn) {
        Ok(ts) => ts.into(),
        Err(e) => e.write_errors().into(),
    }
}

fn expand(attr: &LuaFnAttr, item_fn: &mut ItemFn) -> darling::Result<TokenStream2> {
    let fn_name = item_fn.sig.ident.clone();
    let fn_docs = extract_docs(&item_fn.attrs);

    // Lua FFI requires concrete types. A generic / where-bounded fn
    // would expand into a `*_SPEC` const referencing type parameters
    // that have no value at const-init time — the resulting error is
    // far from the macro site. Reject up front with a clear message.
    if !item_fn.sig.generics.params.is_empty() {
        return Err(darling::Error::custom(
            "#[lua_fn] does not support generic functions — Lua FFI requires concrete types",
        )
        .with_span(&item_fn.sig.generics.span()));
    }
    if let Some(wc) = &item_fn.sig.generics.where_clause {
        return Err(darling::Error::custom(
            "#[lua_fn] does not support `where` clauses — Lua FFI requires concrete types",
        )
        .with_span(&wc.span()));
    }

    // `#[lua_fn]` signature constraints are determined by mlua's
    // `FromLuaMulti` (owned params; `&str`/`&Table` don't satisfy the
    // 'static-ish lifetime bound) and by the wrapper closure that the
    // macro emits (always `LuaResult<T>` so it can compose with `?`).
    // Both can fire `clippy::needless_pass_by_value` or
    // `clippy::unnecessary_wraps` on user-written fns even when the
    // user can't actually change the signature. Silence them on the
    // fn the macro processes — scoped to the macro's footprint,
    // never visible to the rest of the app code.
    //
    // `used_underscore_binding` fires when a user fn takes an
    // intentionally-ignored param like `_slug: String` (e.g. the pool
    // variant of a no-op handler) — the macro's wrapper passes the
    // arg through, so clippy sees the underscore-prefixed binding as
    // "used."
    item_fn.attrs.push(syn::parse_quote! {
        #[allow(
            clippy::needless_pass_by_value,
            clippy::unnecessary_wraps,
            clippy::used_underscore_binding,
            // `state: &()` for a no-op pool-variant handler.
            clippy::trivially_copy_pass_by_ref,
        )]
    });

    let (state_ty, lua_args) = analyze_inputs(&item_fn.sig.inputs)?;

    let mut param_decls: Vec<TokenStream2> = Vec::new();
    let mut closure_arg_names: Vec<proc_macro2::Ident> = Vec::new();
    let mut closure_arg_types: Vec<Type> = Vec::new();

    for arg in &lua_args {
        let info = extract_param_info(arg)?;
        let name = info.name.to_string();
        let lua_ty = if let Some(over) = &info.attr.ty {
            over.clone()
        } else {
            map_type_to_string(&info.ty)?
        };
        let doc = info.attr.doc.clone().unwrap_or_default();
        param_decls.push(quote! {
            LuaParam { name: #name, ty: #lua_ty, doc: #doc }
        });
        closure_arg_names.push(info.name.clone());
        closure_arg_types.push(info.ty.clone());
    }

    let returns_decl = build_returns_decl(
        &item_fn.sig.output,
        attr.returns.as_deref(),
        attr.returns_doc.as_deref(),
    )?;

    let path = &attr.path;
    let doc_lines: Vec<TokenStream2> = fn_docs.iter().map(|s| quote! { #s }).collect();

    let spec_const_name = format_ident!("{}_SPEC", fn_name.to_string().to_uppercase());
    let register_fn_name = format_ident!("{}_register", fn_name);

    // Strip per-arg `#[lua(...)]` attrs from the function so re-emission
    // is clean.
    strip_lua_param_attrs(item_fn);

    // Build the inner call:
    // `fn_name(&state, lua, a, b, c)` (stateful) or `fn_name(lua, a, b, c)` (stateless).
    let raw_call = if state_ty.is_some() {
        quote! { #fn_name(&state, lua, #(#closure_arg_names),*) }
    } else {
        quote! { #fn_name(lua, #(#closure_arg_names),*) }
    };

    // `auto_tx`: route through `with_lua_db` so the user fn's
    // `get_tx_conn(lua)?` works transparently in both hook-mode
    // (existing shared TxContext) and job-mode (PoolContext →
    // per-op IMMEDIATE tx). The `_conn` arg is unused — user code
    // pulls the conn from `get_tx_conn` as before.
    let inner_call = if attr.auto_tx {
        quote! {
            ::crap_cms::hooks::lua_api::crud::with_lua_db(lua, |_conn| { #raw_call })
        }
    } else {
        raw_call
    };
    let closure_body = build_closure_body(&inner_call, &closure_arg_names, &closure_arg_types);

    let state_param_ty = if let Some(t) = &state_ty {
        quote! { ::std::sync::Arc<#t> }
    } else {
        quote! { ::std::sync::Arc<()> }
    };

    let expanded = quote! {
        #item_fn

        #[allow(non_upper_case_globals)]
        pub(crate) const #spec_const_name: LuaFnSpec = LuaFnSpec {
            path: #path,
            doc: &[#(#doc_lines),*],
            params: &[#(#param_decls),*],
            returns: #returns_decl,
        };

        // The wrapper closure that `lua.create_function` consumes uses
        // the user fn's parameter names verbatim. When a user fn marks
        // an arg as intentionally unused (`_slug: String`), the
        // generated closure body reads that binding to forward it to
        // the user fn — `clippy::used_underscore_binding` then fires
        // on the wrapper. Silence it on the wrapper alone; the user
        // fn's body still gets normal lints applied.
        // `trivially_copy_pass_by_ref` fires on `_state: &()` for the
        // pool-variant no-op handlers — same reasoning.
        #[allow(
            non_snake_case,
            dead_code,
            clippy::used_underscore_binding,
            clippy::trivially_copy_pass_by_ref,
        )]
        pub(crate) fn #register_fn_name(
            lua: &::mlua::Lua,
            state: #state_param_ty,
        ) -> ::mlua::Result<::mlua::Function> {
            let _ = &state; // suppress unused-state warning in stateless wrappers
            #closure_body
        }
    };

    Ok(expanded)
}

struct LuaArgInfo {
    name: proc_macro2::Ident,
    ty: Type,
    attr: LuaParamAttr,
}

/// Analyze a function's input list. The first arg is either `&Lua`
/// (stateless) or `&State` (stateful). All remaining args are
/// Lua-facing parameters.
///
/// Returns `(Option<state_ty>, Vec<lua_arg>)`. `state_ty` is `None` when
/// the first arg is `&Lua`.
fn analyze_inputs(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::Token![,]>,
) -> darling::Result<(Option<Type>, Vec<&PatType>)> {
    let mut iter = inputs.iter();
    let first = iter.next().ok_or_else(|| {
        darling::Error::custom("lua_fn requires at least one parameter (&Lua or &State)")
    })?;

    let first_typed = expect_typed(first)?;
    let state_ty = classify_first_arg(&first_typed.ty)?;

    let mut lua_args: Vec<&PatType> = Vec::new();
    for arg in iter {
        let typed = expect_typed(arg)?;
        lua_args.push(typed);
    }

    // If first arg was state (not &Lua), the SECOND arg MUST be &Lua.
    if state_ty.is_some() {
        let lua_arg = lua_args.first().ok_or_else(|| {
            darling::Error::custom(
                "stateful #[lua_fn] needs `&Lua` as second parameter (after state)",
            )
        })?;
        if classify_first_arg(&lua_arg.ty)?.is_some() {
            return Err(darling::Error::custom(
                "second parameter of a stateful #[lua_fn] must be `&Lua`",
            )
            .with_span(&lua_arg.span()));
        }
        // Drop the &Lua arg; what remains is the actual Lua-facing params.
        lua_args.remove(0);
    }

    Ok((state_ty, lua_args))
}

fn expect_typed(arg: &FnArg) -> darling::Result<&PatType> {
    match arg {
        FnArg::Typed(pt) => Ok(pt),
        FnArg::Receiver(r) => Err(darling::Error::custom(
            "#[lua_fn] doesn't support `self` receivers — use a free function",
        )
        .with_span(&r.span())),
    }
}

/// Classify a reference-typed first arg. Returns `None` if it's `&Lua` /
/// `&mlua::Lua` (stateless), `Some(inner_type)` if it's `&Something`
/// (state). Errors on non-reference types.
fn classify_first_arg(ty: &Type) -> darling::Result<Option<Type>> {
    let Type::Reference(r) = ty else {
        return Err(darling::Error::custom(format!(
            "first parameter must be `&Lua` (stateless) or `&State` (stateful), got `{}`",
            quote!(#ty)
        ))
        .with_span(&ty.span()));
    };
    let inner = &*r.elem;
    if is_lua_type(inner) {
        Ok(None)
    } else {
        Ok(Some(inner.clone()))
    }
}

fn is_lua_type(ty: &Type) -> bool {
    let Type::Path(p) = ty else { return false };
    let Some(last) = p.path.segments.last() else {
        return false;
    };
    last.ident == "Lua"
}

fn extract_param_info(pt: &PatType) -> darling::Result<LuaArgInfo> {
    let Pat::Ident(pi) = &*pt.pat else {
        return Err(darling::Error::custom(
            "#[lua_fn] parameters must be named (no destructuring or `_`)",
        )
        .with_span(&pt.pat.span()));
    };
    let attr = parse_param_attr(&pt.attrs)?;
    Ok(LuaArgInfo {
        name: pi.ident.clone(),
        ty: (*pt.ty).clone(),
        attr,
    })
}

fn parse_param_attr(attrs: &[Attribute]) -> darling::Result<LuaParamAttr> {
    for attr in attrs {
        if !attr.path().is_ident("lua") {
            continue;
        }
        return LuaParamAttr::from_meta(&attr.meta);
    }
    Ok(LuaParamAttr::default())
}

/// Build the closure that `lua.create_function` consumes.
///
/// For zero args: `lua.create_function(move |lua, ()| inner_call())`.
/// For one arg : `lua.create_function(move |lua, a0: T0| inner_call(a0))`.
/// For N args  : `lua.create_function(move |lua, (a0, a1, ...): (T0, T1, ...)| inner_call(a0, a1, ...))`.
fn build_closure_body(
    inner_call: &TokenStream2,
    arg_names: &[proc_macro2::Ident],
    arg_types: &[Type],
) -> TokenStream2 {
    let closure_pat = match arg_names.len() {
        0 => quote! { _: () },
        1 => {
            let n = &arg_names[0];
            let t = &arg_types[0];
            quote! { #n: #t }
        }
        _ => {
            quote! { ( #(#arg_names),* ): ( #(#arg_types),* ) }
        }
    };

    quote! {
        lua.create_function(move |lua, #closure_pat| #inner_call)
    }
}

/// Map a Rust return type to a `LuaReturn` initializer expression.
///
/// - `()` or no return → `None`
/// - `LuaResult<()>` / `mlua::Result<()>` / `Result<(), _>` → `None`
/// - `LuaResult<T>` / `mlua::Result<T>` / `Result<T, _>` → `Some(LuaReturn { ty: <T>, doc })`
/// - `T` (bare) → `Some(LuaReturn { ty: <T>, doc })`
///
/// An explicit `#[lua_fn(returns = "...")]` override wins over the
/// inferred type. `returns_doc` is the optional `--- @return ty  <doc>`
/// trailing prose.
fn build_returns_decl(
    ret: &ReturnType,
    override_ty: Option<&str>,
    returns_doc: Option<&str>,
) -> darling::Result<TokenStream2> {
    let doc = returns_doc.unwrap_or("");
    if let Some(ty_str) = override_ty {
        return Ok(quote! { Some(LuaReturn { ty: #ty_str, doc: #doc }) });
    }

    let ty = match ret {
        ReturnType::Default => return Ok(quote! { None }),
        ReturnType::Type(_, ty) => &**ty,
    };

    // Unwrap Result<T, _> / LuaResult<T> / mlua::Result<T> → T
    let inner = unwrap_result(ty).unwrap_or(ty);

    // () → None
    if is_unit(inner) {
        return Ok(quote! { None });
    }

    let ty_str = map_type_to_string(inner)?;
    Ok(quote! { Some(LuaReturn { ty: #ty_str, doc: #doc }) })
}

fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn unwrap_result(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    let name = seg.ident.to_string();
    if name != "Result" && name != "LuaResult" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let first = args.args.first()?;
    let syn::GenericArgument::Type(t) = first else {
        return None;
    };
    Some(t)
}

/// Strip `#[lua(...)]` attributes off function parameters. They've
/// already been consumed by `parse_param_attr`; leaving them in the
/// re-emitted fn body would trip `cfg(not(macro))` builds.
fn strip_lua_param_attrs(item_fn: &mut ItemFn) {
    for arg in &mut item_fn.sig.inputs {
        let FnArg::Typed(pt) = arg else { continue };
        pt.attrs.retain(|a| !a.path().is_ident("lua"));
    }
}

/// Map a Rust type to its Lua type string. Used by `#[lua_fn]` for
/// argument / return types AND by `LuaAlias`'s newtype-variant
/// type-union mode.
///
/// Note: this returns a `String` literal at macro-expansion time. Types
/// that need runtime resolution (e.g. nested `LuaAnnotation` classes)
/// aren't supported here — use `#[lua_fn(returns = "...")]` or
/// `#[lua(ty = "...")]` on the param for those.
pub(crate) fn map_type_to_string(ty: &Type) -> darling::Result<String> {
    let ty = strip_ref(ty);

    // Option<T> → "T?"
    if let Some(inner) = unwrap_path("Option", ty) {
        let inner_str = map_type_to_string(inner)?;
        return Ok(format!("{inner_str}?"));
    }

    // Vec<T> → "T[]"
    if let Some(inner) = unwrap_path("Vec", ty) {
        let inner_str = map_type_to_string(inner)?;
        return Ok(format!("{inner_str}[]"));
    }

    // HashMap<String, V> → "table<string, V>"
    if let Some((k, v)) = unwrap_pair_path("HashMap", ty) {
        let k_str = map_type_to_string(k)?;
        let v_str = map_type_to_string(v)?;
        return Ok(format!("table<{k_str}, {v_str}>"));
    }

    // Bare scalar / mlua-specific
    let Type::Path(p) = ty else {
        return Err(darling::Error::custom(format!(
            "cannot auto-map type `{}` — add #[lua(ty = \"...\")] on the param",
            quote!(#ty)
        ))
        .with_span(&ty.span()));
    };
    let Some(seg) = p.path.segments.last() else {
        return Err(darling::Error::custom(format!(
            "cannot auto-map empty path `{}`",
            quote!(#ty)
        ))
        .with_span(&ty.span()));
    };
    let name = seg.ident.to_string();

    Ok(match name.as_str() {
        "String" | "str" => "string".into(),
        "bool" => "boolean".into(),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128"
        | "isize" => "integer".into(),
        "f32" | "f64" => "number".into(),
        // mlua-specific
        "Table" => "table".into(),
        "Value" => "any".into(),
        "Function" => "function".into(),
        // Fall through: assume the user knows what they're doing — emit
        // as-is. This is the path that requires `#[lua(ty = "...")]`
        // for non-trivial cases since we can't resolve
        // `<T as LuaAnnotation>::CLASS_NAME` at const-init time
        // (returns a `&'static str`, not const-friendly enough for our
        // generated const init).
        other => {
            return Err(darling::Error::custom(format!(
                "type `{other}` does not auto-map to a Lua type; add #[lua(ty = \"...\")] on the param or #[lua_fn(returns = \"...\")] on the fn"
            ))
            .with_span(&ty.span()));
        }
    })
}

/// Like `unwrap_path` but extracts two generic args (for `HashMap<K, V>`).
fn unwrap_pair_path<'a>(target: &str, ty: &'a Type) -> Option<(&'a Type, &'a Type)> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != target {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let mut iter = args.args.iter();
    let first = iter.next()?;
    let second = iter.next()?;
    let (syn::GenericArgument::Type(t1), syn::GenericArgument::Type(t2)) = (first, second) else {
        return None;
    };
    Some((t1, t2))
}
