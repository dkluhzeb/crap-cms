//! Job and scheduler types: definitions, runs, and status tracking.

pub mod definition;
pub mod labels;
pub mod run;
pub mod status;
pub mod system;

pub use definition::{JobDefinition, JobDefinitionBuilder};
pub use labels::JobLabels;
pub use run::{JobRun, JobRunBuilder};
pub use status::JobStatus;
pub use system::{
    SYSTEM_BULK_JOB, SYSTEM_BULK_QUEUE, SYSTEM_EMAIL_JOB, SYSTEM_IMAGE_CONVERT_JOB,
    SYSTEM_JOB_SLUGS,
};
