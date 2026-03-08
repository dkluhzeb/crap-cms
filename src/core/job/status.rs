use serde::{Deserialize, Serialize};

/// Status of a job run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stale,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Stale => "stale",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(JobStatus::Pending),
            "running" => Some(JobStatus::Running),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            "stale" => Some(JobStatus::Stale),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_status_as_str_all_variants() {
        assert_eq!(JobStatus::Pending.as_str(), "pending");
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Completed.as_str(), "completed");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
        assert_eq!(JobStatus::Stale.as_str(), "stale");
    }

    #[test]
    fn job_status_from_str_valid() {
        assert_eq!(JobStatus::from_str("pending"), Some(JobStatus::Pending));
        assert_eq!(JobStatus::from_str("running"), Some(JobStatus::Running));
        assert_eq!(JobStatus::from_str("completed"), Some(JobStatus::Completed));
        assert_eq!(JobStatus::from_str("failed"), Some(JobStatus::Failed));
        assert_eq!(JobStatus::from_str("stale"), Some(JobStatus::Stale));
    }

    #[test]
    fn job_status_from_str_invalid() {
        assert_eq!(JobStatus::from_str("unknown"), None);
        assert_eq!(JobStatus::from_str(""), None);
        assert_eq!(JobStatus::from_str("PENDING"), None);
    }

    #[test]
    fn job_status_roundtrip() {
        for status in &[
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Stale,
        ] {
            let s = status.as_str();
            let parsed = JobStatus::from_str(s).expect("should roundtrip");
            assert_eq!(&parsed, status);
        }
    }
}
