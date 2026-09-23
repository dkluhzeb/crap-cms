//! Where a job run came from — the `scheduled_by` provenance every queued run
//! records.

/// The surface that queued a job run. A closed set: every insert names one of
/// these, stored as [`ScheduledBy::as_str`] in `_crap_jobs.scheduled_by` and
/// surfaced to Lua (`ctx.job.scheduled_by`), MCP, the CLI and gRPC
/// (`JobScheduledBy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledBy {
    /// Queued through the gRPC API.
    Grpc,
    /// Fired by a job's cron schedule.
    Cron,
    /// Queued from Lua (`crap.jobs.queue`).
    Hook,
    /// Queued through an MCP tool.
    Mcp,
    /// Queued from the command line (`crap-cms jobs trigger`).
    Cli,
    /// Queued by the CMS itself: email delivery, image conversion, the
    /// migration drain.
    System,
}

impl ScheduledBy {
    /// Every provenance. Tests pin exhaustiveness against it: each variant's
    /// stored name round-trips, and every wire mapping covers all of them.
    pub const ALL: [ScheduledBy; 6] = [
        ScheduledBy::Grpc,
        ScheduledBy::Cron,
        ScheduledBy::Hook,
        ScheduledBy::Mcp,
        ScheduledBy::Cli,
        ScheduledBy::System,
    ];

    /// The stored and surfaced name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScheduledBy::Grpc => "grpc",
            ScheduledBy::Cron => "cron",
            ScheduledBy::Hook => "hook",
            ScheduledBy::Mcp => "mcp",
            ScheduledBy::Cli => "cli",
            ScheduledBy::System => "system",
        }
    }

    /// Read a stored name back. Tolerates the one legacy spelling earlier
    /// releases wrote: `"api"`, recorded for every queued bulk operation
    /// (which only gRPC could queue then). Any other unknown value — rows
    /// written by hand, or `"manual"` — is `None`.
    #[must_use]
    pub fn from_stored(stored: &str) -> Option<Self> {
        match stored {
            "grpc" | "api" => Some(ScheduledBy::Grpc),
            "cron" => Some(ScheduledBy::Cron),
            "hook" => Some(ScheduledBy::Hook),
            "mcp" => Some(ScheduledBy::Mcp),
            "cli" => Some(ScheduledBy::Cli),
            "system" => Some(ScheduledBy::System),
            _ => None,
        }
    }

    /// The name a stored value reads back as: its canonical spelling when it
    /// names a provenance (so a legacy `"api"` row reads as `"grpc"`), the
    /// stored text unchanged otherwise.
    #[must_use]
    pub fn canonical_name(stored: String) -> String {
        match Self::from_stored(&stored) {
            Some(by) => by.as_str().to_string(),
            None => stored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provenance_round_trips_through_its_stored_name() {
        for by in ScheduledBy::ALL {
            assert_eq!(ScheduledBy::from_stored(by.as_str()), Some(by));
            assert_eq!(ScheduledBy::canonical_name(by.as_str().into()), by.as_str());
        }
    }

    #[test]
    fn stored_names_are_pinned() {
        assert_eq!(
            ScheduledBy::ALL.map(ScheduledBy::as_str),
            ["grpc", "cron", "hook", "mcp", "cli", "system"]
        );
    }

    /// Earlier releases recorded a queued bulk operation as `"api"`.
    #[test]
    fn the_legacy_api_spelling_reads_as_grpc() {
        assert_eq!(ScheduledBy::from_stored("api"), Some(ScheduledBy::Grpc));
        assert_eq!(ScheduledBy::canonical_name("api".into()), "grpc");
    }

    #[test]
    fn an_unknown_stored_value_is_kept_as_it_is() {
        assert_eq!(ScheduledBy::from_stored("manual"), None);
        assert_eq!(ScheduledBy::from_stored(""), None);
        assert_eq!(ScheduledBy::from_stored("GRPC"), None);
        assert_eq!(ScheduledBy::canonical_name("manual".into()), "manual");
    }
}
