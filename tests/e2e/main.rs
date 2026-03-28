#[allow(dead_code)]
mod helpers;
#[allow(dead_code)]
mod html;

mod html_auth;
mod html_crud;
mod html_forms;
mod html_globals;
mod html_locale;
mod html_nesting;
mod html_pagination;
mod html_validation;
mod html_versions;

#[cfg(feature = "browser-tests")]
mod browser;
#[cfg(feature = "browser-tests")]
mod browser_array;
#[cfg(feature = "browser-tests")]
mod browser_blocks;
#[cfg(feature = "browser-tests")]
mod browser_code;
#[cfg(feature = "browser-tests")]
mod browser_collapsible;
#[cfg(feature = "browser-tests")]
mod browser_confirm;
#[cfg(feature = "browser-tests")]
mod browser_dirty_form;
#[cfg(feature = "browser-tests")]
mod browser_focal_point;
#[cfg(feature = "browser-tests")]
mod browser_list_settings;
#[cfg(feature = "browser-tests")]
mod browser_locale;
#[cfg(feature = "browser-tests")]
mod browser_relationship;
#[cfg(feature = "browser-tests")]
mod browser_richtext;
#[cfg(feature = "browser-tests")]
mod browser_sidebar;
#[cfg(feature = "browser-tests")]
mod browser_tabs;
#[cfg(feature = "browser-tests")]
mod browser_tags;
#[cfg(feature = "browser-tests")]
mod browser_theme;
#[cfg(feature = "browser-tests")]
mod browser_time;
#[cfg(feature = "browser-tests")]
mod browser_toast;
#[cfg(feature = "browser-tests")]
mod browser_validation;
