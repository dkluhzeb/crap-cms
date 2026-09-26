//! Every stored value of a has-many list is a list.
//!
//! A `text`, `number`, `select` or `radio` field with `has_many = true` stores
//! its values as a JSON array — in its column, or inside a JSON-stored row — and
//! so does a has-many relationship or upload inside an array or blocks row (its
//! id list). Filters expand that array element by element, trusting it to hold
//! NULL or the list a write stores. Writes always store that form. Only older
//! data or a change to a definition leaves other values behind: a field switched
//! to `has_many` over its single values, a list whose element type changed (a
//! text list turned into a number list), a Postgres number column reconciled to
//! text, or a reference list an earlier admin form stored as comma-separated
//! ids.
//!
//! This pass brings those values to the list form whenever the has-many fields
//! of a collection or global change shape: a single value becomes a one-element
//! list, a list's elements take the field's type, blank text becomes NULL — the
//! reading [`stored_list`](crate::db::query::helpers::stored_list) shares with
//! the write. Text that isn't a JSON array reads by where it is stored: in a
//! document's own column (top-level, per locale, a group's prefixed column) it
//! is one value — `"Hello, world"` becomes `["Hello, world"]` — since no
//! release stored a list there in any other form; inside an array or blocks row
//! (an array table's column, a row's JSON) it is comma-separated values, the
//! form earlier admin forms stored a row's list in. A value holding nothing of
//! the field's type (text in a number list, a polymorphic entry that isn't
//! `collection/id`) can't become a list without losing it, so startup stops and
//! names the documents instead.
//!
//! The same pass keeps a has-one relationship or upload inside a row holding
//! a single id: a field switched from `has_many` back to one value leaves its
//! one-element lists behind, which no reader of a has-one reference reads (the
//! reference would stop counting, leaving its target deletable while the row
//! still names it). A one-element list becomes its element, an empty one NULL;
//! a list of several can't become one value without dropping the others, so
//! startup stops and names the rows instead.
//!
//! Unlike the one-time conversions this pass stays: any later change to a
//! definition can leave such values behind again.

mod columns;
mod pass;
mod values;

pub(in crate::db::migrate) use pass::normalize_if_needed;
