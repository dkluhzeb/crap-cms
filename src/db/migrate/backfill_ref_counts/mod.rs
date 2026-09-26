//! Backfill of `_ref_count` columns from existing relationship data: the gate
//! that says whether the stored counts are current, and the recount that
//! makes them so.

mod recount;
mod topology;

#[cfg(test)]
mod test_support;

pub(crate) use recount::{backfill_if_needed, invalidate_ref_counts, recompute_ref_counts};
