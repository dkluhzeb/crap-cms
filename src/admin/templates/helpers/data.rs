//! `{{data "name"}}` helper — calls a Lua function registered via
//! `crap.template_data.register(name, fn)` and returns its result for
//! template use.
//!
//! ## Lua side
//!
//! ```lua
//! crap.template_data.register("fetch_weather", function()
//!   return { temp = 22, condition = "sunny" }
//! end)
//! ```
//!
//! ## Template side
//!
//! ```hbs
//! {{#with (data "fetch_weather")}}
//!   <p>{{temp}}°C, {{condition}}</p>
//! {{/with}}
//! ```
//!
//! The function runs **on demand** — only when a rendering template
//! actually evaluates the `{{data}}` call. Pages that don't reference it
//! pay no cost.

use std::sync::Arc;

use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

use crate::hooks::HookRunner;

/// Handlebars helper that invokes a registered Lua template-data
/// function. Returns the function's JSON-converted result, or `null`
/// when no function is registered (or it errors).
pub(super) struct DataHelper {
    pub(super) runner: Arc<HookRunner>,
}

impl HelperDef for DataHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let name = h
            .param(0)
            .and_then(|p| p.value().as_str().map(str::to_string))
            .ok_or_else(|| {
                RenderError::from(handlebars::RenderErrorReason::ParamNotFoundForIndex(
                    "data", 0,
                ))
            })?;

        let result = self
            .runner
            .call_template_data(&name, ctx.data())
            .unwrap_or(Value::Null);

        Ok(ScopedJson::Derived(result))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use handlebars::Handlebars;
    use serde_json::json;

    use crate::{config::CrapConfig, hooks::HookRunner};

    use super::DataHelper;

    fn build_runner(tmp: &tempfile::TempDir, lua_setup: &str) -> HookRunner {
        if !lua_setup.is_empty() {
            std::fs::write(tmp.path().join("init.lua"), lua_setup).unwrap();
        }

        let config = CrapConfig::default();
        HookRunner::builder()
            .config_dir(tmp.path())
            .registry(crate::core::Registry::shared())
            .config(&config)
            .build()
            .expect("runner")
    }

    /// End-to-end test: register a Lua function via `crap.template_data.register`,
    /// invoke it through `{{data "name"}}` in a template, verify result.
    #[test]
    fn data_helper_invokes_registered_lua_function() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(
            &tmp,
            r#"
            crap.template_data.register("widget", function()
              return { title = "Hello", count = 7 }
            end)
            "#,
        );

        let mut hbs = Handlebars::new();
        hbs.register_helper(
            "data",
            Box::new(DataHelper {
                runner: Arc::new(runner),
            }),
        );

        hbs.register_template_string(
            "page",
            r#"{{#with (data "widget")}}{{title}}={{count}}{{/with}}"#,
        )
        .unwrap();

        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "Hello=7");
    }

    #[test]
    fn data_helper_returns_null_for_missing_function() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(&tmp, "");

        let mut hbs = Handlebars::new();
        hbs.register_helper(
            "data",
            Box::new(DataHelper {
                runner: Arc::new(runner),
            }),
        );

        // No `widget` registered → null → block body skipped.
        hbs.register_template_string("page", r#"({{#with (data "widget")}}YES{{/with}})"#)
            .unwrap();

        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "()");
    }

    /// The Lua function receives the full page context as its first argument,
    /// so widgets can scope themselves by user, document, page type, etc.
    #[test]
    fn data_helper_passes_page_context_to_lua() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(
            &tmp,
            r#"
            crap.template_data.register("scoped_widget", function(ctx)
              return {
                user_email = ctx.user and ctx.user.email or "anon",
                page_type = ctx.page and ctx.page.type or "unknown",
              }
            end)
            "#,
        );

        let mut hbs = Handlebars::new();
        hbs.register_helper(
            "data",
            Box::new(DataHelper {
                runner: Arc::new(runner),
            }),
        );

        hbs.register_template_string(
            "page",
            r#"{{#with (data "scoped_widget")}}{{user_email}}@{{page_type}}{{/with}}"#,
        )
        .unwrap();

        let ctx = json!({
            "user": { "email": "alice@example.com" },
            "page": { "type": "dashboard" },
        });
        let result = hbs.render("page", &ctx).unwrap();
        assert_eq!(result, "alice@example.com@dashboard");
    }

    /// Functions registered with no parameters keep working — Lua drops the
    /// extra context argument silently.
    #[test]
    fn data_helper_tolerates_zero_arg_lua_functions() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(
            &tmp,
            r#"
            crap.template_data.register("static_widget", function()
              return "OK"
            end)
            "#,
        );

        let mut hbs = Handlebars::new();
        hbs.register_helper(
            "data",
            Box::new(DataHelper {
                runner: Arc::new(runner),
            }),
        );

        hbs.register_template_string("page", r#"{{data "static_widget"}}"#)
            .unwrap();

        let result = hbs
            .render("page", &json!({"user": {"email": "x"}}))
            .unwrap();
        assert_eq!(result, "OK");
    }
}
