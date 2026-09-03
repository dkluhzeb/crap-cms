//! The built-in slot registry — the SINGLE source for the slots-guide table
//! (`docs/src/admin-ui/guides/slots.md`, injected by `cargo xtask
//! gen-doc-tables`) and for the stable-API test pinning every slot to its
//! declaring template. Adding or renaming a slot means editing this table;
//! both the docs and the pin follow automatically.

/// One documented built-in slot.
pub struct SlotDoc {
    /// Template file (relative to `templates/`) declaring the slot.
    pub file: &'static str,
    /// Slot name — referenced by user overlays; a stable API.
    pub slot: &'static str,
    /// Render context available inside the slot.
    pub context: &'static str,
    /// What the slot is for (doc prose).
    pub use_for: &'static str,
}

/// Every built-in slot, in guide order.
pub static SLOT_DOCS: &[SlotDoc] = &[
    SlotDoc {
        file: "layout/base.hbs",
        slot: "head_extras",
        context: "full page context",
        use_for: "extra `<meta>` tags, OG tags, robots directives, PWA `<link rel=\"manifest\">`, custom `<link rel=\"preconnect\">`, analytics `<script>`",
    },
    SlotDoc {
        file: "layout/base.hbs",
        slot: "body_end_scripts",
        context: "full page context",
        use_for: "end-of-body analytics, custom event listeners, third-party scripts that should load after the admin",
    },
    SlotDoc {
        file: "layout/header.hbs",
        slot: "page_header_actions",
        context: "full page context",
        use_for: "extra buttons in the top header bar (next to the logout button)",
    },
    SlotDoc {
        file: "layout/sidebar.hbs",
        slot: "sidebar_bottom",
        context: "nav context",
        use_for: "extra navigation links pinned to the bottom of the left sidebar",
    },
    SlotDoc {
        file: "dashboard/index.hbs",
        slot: "dashboard_widgets",
        context: "dashboard context",
        use_for: "custom dashboard cards (recent activity, system status, weather, queue depth, …)",
    },
    SlotDoc {
        file: "collections/edit_form.hbs",
        slot: "collection_edit_toolbar",
        context: "edit-form context (`document`, `collection`, `user`)",
        use_for: "extra toolbar actions on collection edit pages (e.g., a \"Preview\" button)",
    },
    SlotDoc {
        file: "collections/edit_sidebar.hbs",
        slot: "collection_edit_sidebar",
        context: "edit-form context",
        use_for: "extra sidebar panels on collection edit pages (related items, audit log, custom metadata)",
    },
    SlotDoc {
        file: "collections/items.hbs",
        slot: "list_toolbar_actions",
        context: "list context (`collection`, `docs`, `pagination`, `user`)",
        use_for: "extra buttons in the list toolbar, next to Filters/Columns (export, bulk tools, custom views)",
    },
    SlotDoc {
        file: "collections/items.hbs",
        slot: "list_footer",
        context: "list context",
        use_for: "content below the list table/pagination (summaries, legends, totals). Rendered **only when the list has rows** — it sits inside the `{{#if docs}}` branch, so an empty (filtered) result shows no footer",
    },
    SlotDoc {
        file: "globals/edit.hbs",
        slot: "global_edit_toolbar",
        context: "global edit context (`global`, `user`)",
        use_for: "extra toolbar actions on global edit pages — parity with `collection_edit_toolbar`",
    },
    SlotDoc {
        file: "globals/edit_sidebar.hbs",
        slot: "global_edit_sidebar",
        context: "global edit context",
        use_for: "extra sidebar panels on global edit pages — parity with `collection_edit_sidebar`",
    },
    SlotDoc {
        file: "auth/login.hbs",
        slot: "login_extras",
        context: "minimal auth context",
        use_for: "additional content on the login page (compliance notices, SSO links, banner messages)",
    },
    SlotDoc {
        file: "partials/breadcrumb.hbs",
        slot: "breadcrumb_extras",
        context: "page context of the page showing the breadcrumb",
        use_for: "extra content at the end of the breadcrumb trail (environment badge, quick links)",
    },
    SlotDoc {
        file: "partials/field.hbs",
        slot: "field_help",
        context: "per-field: `name`, `kind` (field type), `label`",
        use_for: "extra help content rendered under a specific field's input — match on `name` or `kind`",
    },
];

/// Render the slots-guide Markdown table (the generated region of
/// `docs/src/admin-ui/guides/slots.md`).
#[must_use]
pub fn generate_slots_table() -> String {
    use std::fmt::Write as _;

    let mut out = String::from("| Slot | Declared in | Context | Use for |\n|---|---|---|---|\n");

    for s in SLOT_DOCS {
        let _ = writeln!(
            out,
            "| `{}` | `{}` | {} | {} |",
            s.slot, s.file, s.context, s.use_for
        );
    }

    out.trim_end().to_string()
}
