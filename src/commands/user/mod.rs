//! `user` command — user management for auth collections.

mod create;
mod dispatch;
mod helpers;
mod info;
mod list;
mod modify;

pub use create::{UserCreateParams, user_create};
pub use dispatch::run;
pub use list::user_list;
pub use modify::{
    UserChangePasswordParams, UserDeleteParams, user_change_password, user_delete, user_lock,
    user_reset_totp, user_unlock,
};
