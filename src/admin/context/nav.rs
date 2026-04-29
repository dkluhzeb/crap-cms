//! Sidebar navigation context — the lists of collections and globals shown in
//! the left sidebar.
//!
//! Sorted alphabetically by slug; filtered down to entries the current user
//! can read by `BasePageContext::for_handler`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::admin::AdminState;

/// Top-level nav data exposed at `{{nav.*}}`.
#[derive(Serialize, JsonSchema)]
pub struct NavData {
    pub collections: Vec<NavCollection>,
    pub globals: Vec<NavGlobal>,
}

/// One sidebar entry for a collection.
#[derive(Serialize, JsonSchema)]
pub struct NavCollection {
    pub slug: String,
    pub display_name: String,
    pub is_auth: bool,
    pub is_upload: bool,
}

/// One sidebar entry for a global.
#[derive(Serialize, JsonSchema)]
pub struct NavGlobal {
    pub slug: String,
    pub display_name: String,
}

impl NavData {
    /// Build sidebar nav from the registry. Sorted alphabetically by slug.
    pub fn from_state(state: &AdminState) -> Self {
        let mut collections: Vec<NavCollection> = state
            .registry
            .collections
            .values()
            .map(|def| NavCollection {
                slug: def.slug.to_string(),
                display_name: def.display_name().to_string(),
                is_auth: def.is_auth_collection(),
                is_upload: def.is_upload_collection(),
            })
            .collect();
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<NavGlobal> = state
            .registry
            .globals
            .values()
            .map(|def| NavGlobal {
                slug: def.slug.to_string(),
                display_name: def.display_name().to_string(),
            })
            .collect();
        globals.sort_by(|a, b| a.slug.cmp(&b.slug));

        Self {
            collections,
            globals,
        }
    }
}
