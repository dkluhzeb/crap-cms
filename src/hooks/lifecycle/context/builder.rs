//! Builder for [`HookContext`].

use std::collections::HashMap;

use crate::core::Document;

use super::HookContext;

/// Builder for `HookContext`. Created via [`HookContext::builder`].
pub struct HookContextBuilder {
    collection: String,
    operation: String,
    data: HashMap<String, serde_json::Value>,
    locale: Option<String>,
    draft: Option<bool>,
    context: HashMap<String, serde_json::Value>,
    user: Option<Document>,
    ui_locale: Option<String>,
}

impl HookContextBuilder {
    pub(super) fn new(collection: String, operation: String) -> Self {
        Self {
            collection,
            operation,
            data: HashMap::new(),
            locale: None,
            draft: None,
            context: HashMap::new(),
            user: None,
            ui_locale: None,
        }
    }

    pub fn data(mut self, data: HashMap<String, serde_json::Value>) -> Self {
        self.data = data;
        self
    }

    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = Some(locale.into());
        self
    }

    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = Some(draft);
        self
    }

    pub fn context(mut self, context: HashMap<String, serde_json::Value>) -> Self {
        self.context = context;
        self
    }

    pub fn user(mut self, user: Option<&Document>) -> Self {
        self.user = user.cloned();
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&str>) -> Self {
        self.ui_locale = ui_locale.map(|s| s.to_string());
        self
    }

    pub fn build(self) -> HookContext {
        HookContext {
            collection: self.collection,
            operation: self.operation,
            data: self.data,
            locale: self.locale,
            draft: self.draft,
            context: self.context,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn builder_defaults() {
        let ctx = HookContext::builder("posts", "create").build();
        assert_eq!(ctx.collection, "posts");
        assert_eq!(ctx.operation, "create");
        assert!(ctx.data.is_empty());
        assert!(ctx.locale.is_none());
        assert!(ctx.draft.is_none());
        assert!(ctx.context.is_empty());
        assert!(ctx.user.is_none());
        assert!(ctx.ui_locale.is_none());
    }

    #[test]
    fn builder_all_fields() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));
        let mut ctx_map = HashMap::new();
        ctx_map.insert("request_id".to_string(), json!("abc"));

        let ctx = HookContext::builder("posts", "update")
            .data(data)
            .locale("en")
            .draft(true)
            .context(ctx_map)
            .build();

        assert_eq!(ctx.collection, "posts");
        assert_eq!(ctx.operation, "update");
        assert_eq!(ctx.data.get("title"), Some(&json!("Hello")));
        assert_eq!(ctx.locale.as_deref(), Some("en"));
        assert_eq!(ctx.draft, Some(true));
        assert_eq!(ctx.context.get("request_id"), Some(&json!("abc")));
    }

    #[test]
    fn builder_partial() {
        let ctx = HookContext::builder("pages", "delete").draft(false).build();

        assert_eq!(ctx.collection, "pages");
        assert_eq!(ctx.draft, Some(false));
        assert!(ctx.locale.is_none());
        assert!(ctx.data.is_empty());
    }
}
