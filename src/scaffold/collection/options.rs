//! Boolean flags driving `make_collection` output.

/// Boolean flags for collection scaffolding. All-`false` default
/// matches "plain collection, no special flags".
#[derive(Default)]
pub struct CollectionOptions {
    pub no_timestamps: bool,
    pub auth: bool,
    pub upload: bool,
    pub versions: bool,
    pub force: bool,
}
