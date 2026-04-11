//! `user` command — user management for auth collections.

mod create;
mod dispatch;
mod helpers;
mod info;
mod list;
mod modify;

pub use create::user_create;
pub use dispatch::run;
pub use list::user_list;
pub use modify::{
    user_change_password, user_delete, user_lock, user_unlock, user_unverify, user_verify,
};
