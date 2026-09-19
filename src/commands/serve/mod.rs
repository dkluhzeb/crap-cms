//! `serve` command — start admin UI and gRPC servers.

mod pid;
mod process;
mod startup;

pub use process::detach;
#[cfg(unix)]
pub use process::{restart, status, stop};
pub use startup::run;

pub(crate) use pid::{PidFile, refuse_if_running};

pub use startup::ServeMode;
