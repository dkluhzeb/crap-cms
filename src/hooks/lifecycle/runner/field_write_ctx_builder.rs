//! Builder for [`FieldWriteCtx`].

use crate::{core::Document, db::DbConnection};

use super::run::FieldWriteCtx;

/// Builder for [`FieldWriteCtx`]. Created via [`FieldWriteCtx::builder`].
pub struct FieldWriteCtxBuilder<'a> {
    conn: &'a dyn DbConnection,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

impl<'a> FieldWriteCtxBuilder<'a> {
    pub(crate) fn new(conn: &'a dyn DbConnection) -> Self {
        Self {
            conn,
            user: None,
            ui_locale: None,
        }
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn build(self) -> FieldWriteCtx<'a> {
        FieldWriteCtx {
            conn: self.conn,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}
