//! Form parsing helpers: multipart, array fields, upload metadata.

mod composite;
mod form_data;
mod join_data;
mod parse;
mod select_has_many;

pub(crate) use form_data::FormData;
pub(crate) use parse::{FormParseError, parse_form, parse_multipart_form};
