use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// Display labels for a job definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.JobLabels")]
pub struct JobLabels {
    /// Display label for the job (e.g., "Cleanup Expired Posts").
    pub singular: Option<String>,
}
