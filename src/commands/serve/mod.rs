//! `serve` command — start admin UI and gRPC servers.

mod pid;
mod process;
mod startup;

pub use process::detach;
#[cfg(unix)]
pub use process::{restart, status, stop};
pub use startup::run;

pub use startup::ServeMode;
