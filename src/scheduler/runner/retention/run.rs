//! A retention-purge run: its bounded batches, where it resumes, and the
//! documents it has set aside after a failed purge.

use std::{
    collections::{HashMap, HashSet},
    mem,
};

use anyhow::Error;
use tracing::warn;

use crate::service::PurgeEvents;

/// How many trashed candidates one retention-purge batch examines, across all
/// collections. Every purged row's delete event and file keys are held until
/// its batch commits, and the batch holds the write lock throughout, so both
/// stay bounded however much trash has expired; the rest follows in the next
/// batch.
const RETENTION_PURGE_BATCH: usize = 500;

/// How many of one collection's documents may fail to purge in one run before
/// the whole collection is set aside for that run — a failure that hits every
/// row (a broken table, not a broken row) then costs a bounded number of
/// rolled-back batches, not one per row.
pub(super) const MAX_FAILED_ROWS_PER_COLLECTION: usize = 8;

/// Where a retention-purge run resumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Position {
    /// Nothing examined yet.
    Start,
    /// Collections sorting before `slug` are done; `slug` resumes after `id`.
    After { slug: String, id: String },
    /// Every expired candidate was examined.
    Done,
}

/// Where one collection's scan resumes within a run.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Resume {
    /// The run has passed the collection, or set it aside.
    Skip,
    /// From its first candidate.
    FromStart,
    /// After this id.
    After(String),
}

/// A retention purge run as a sequence of bounded batches, each its caller's
/// own IMMEDIATE transaction, walking the retention collections in slug order
/// and each collection's candidates in id order.
///
/// A document whose purge fails is set aside for the rest of the run: the
/// batch it failed in must be rolled back (a failed statement poisons a
/// Postgres transaction), and [`retry_after_failure`](Self::retry_after_failure)
/// tells the caller to re-run that batch without it. It stays trashed and is
/// retried by the next run, so one bad row neither blocks the purge nor is
/// ever purged without its delete event.
pub struct RetentionPurge {
    pub(super) batch_size: usize,
    pub(super) capture: bool,
    pub(super) position: Position,
    pub(super) failed: Failed,
}

/// The documents a run has set aside after a failed purge.
#[derive(Default)]
pub(super) struct Failed {
    /// Set-aside document ids per collection slug.
    pub(super) rows: HashMap<String, HashSet<String>>,
    /// Whether the latest batch failed on a document it has since set aside.
    retry: bool,
}

/// What one committed batch did: the documents it purged, the files the
/// caller deletes and the delete events it publishes once it has committed.
pub struct PurgeBatch {
    pub purged: u64,
    pub files: Vec<String>,
    pub events: PurgeEvents,
}

impl RetentionPurge {
    /// A run from the start, capturing delete events when `capture` is set
    /// (there is an event transport to publish them on).
    #[must_use]
    pub fn new(capture: bool) -> Self {
        Self {
            batch_size: RETENTION_PURGE_BATCH,
            capture,
            position: Position::Start,
            failed: Failed::default(),
        }
    }

    /// Whether every expired candidate has been examined.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.position == Position::Done
    }

    /// Whether the batch that just failed did so on a document now set
    /// aside — the caller rolls it back and runs the batch again without it.
    /// Any other failure ends the run; the next one starts over.
    pub fn retry_after_failure(&mut self) -> bool {
        mem::take(&mut self.failed.retry)
    }

    /// Where the scan of collection `slug` resumes in this run.
    pub(super) fn resume_in(&self, slug: &str) -> Resume {
        if self.is_set_aside(slug) {
            return Resume::Skip;
        }

        match &self.position {
            Position::Start => Resume::FromStart,
            Position::After { slug: at, id } if at == slug => Resume::After(id.clone()),
            Position::After { slug: at, .. } if at.as_str() < slug => Resume::FromStart,
            Position::After { .. } | Position::Done => Resume::Skip,
        }
    }

    fn is_set_aside(&self, slug: &str) -> bool {
        self.failed
            .rows
            .get(slug)
            .is_some_and(|ids| ids.len() >= MAX_FAILED_ROWS_PER_COLLECTION)
    }

    /// Set document `slug`/`id` aside for the rest of the run after `error`.
    pub(super) fn set_aside(&mut self, slug: &str, id: &str, error: &Error) {
        warn!(
            "Retention purge of {slug}/{id} failed; leaving it trashed until the next \
             purge run: {error:#}"
        );

        let ids = self.failed.rows.entry(slug.to_string()).or_default();
        ids.insert(id.to_string());

        if ids.len() >= MAX_FAILED_ROWS_PER_COLLECTION {
            warn!(
                "Retention purge: {} documents of '{slug}' failed; skipping the \
                 collection until the next purge run",
                ids.len()
            );
        }

        self.failed.retry = true;
    }
}

#[cfg(all(test, feature = "sqlite"))]
impl RetentionPurge {
    /// The run with batches of `batch_size` candidates.
    #[must_use]
    pub(super) fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }
}
