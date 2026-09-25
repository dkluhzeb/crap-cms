//! What `blueprint save` leaves out of a blueprint.
//!
//! A blueprint is a reusable, shareable template, so it must never carry the
//! project's state or secrets. Excluded are:
//!
//! - the runtime directories at the top level: `data/` (database, generated
//!   auth secret, logs, locks), `uploads/`, `types/` (regenerated on use) and
//!   `backups/` (the default `backup` output);
//! - the configured database file and its `-wal` / `-shm` / `-journal`
//!   sidecars, and the configured log directory, wherever inside the project
//!   `crap.toml` puts them;
//! - anywhere in the tree: any `SQLite` database file (and its sidecars), any
//!   backup directory (`manifest.json` beside `crap.db`, e.g. a `backup
//!   --output` inside the project), generated-secret files (`.jwt_secret*`)
//!   and in-flight upload staging files (`*.crap-tmp`).
//!
//! Symlinks are never followed or copied (see
//! [`copy_dir_recursive`](super::helpers::copy_dir_recursive)).

use std::{
    fs::File,
    io::Read as _,
    path::{Path, PathBuf},
};

use crate::{config::CrapConfig, core::upload::is_staging_file_name};

/// Runtime directories skipped at the top level of the project.
const TOP_LEVEL_SKIP: &[&str] = &["data", "uploads", "types", "backups"];

/// Suffixes of `SQLite` sidecar files.
const SQLITE_SIDECARS: &[&str] = &["-wal", "-shm", "-journal"];

/// First 16 bytes of every `SQLite` 3 database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Name prefix of the generated auth secret and the copies a restore keeps.
const SECRET_PREFIX: &str = ".jwt_secret";

/// The exclusion rules for one project.
pub(super) struct SaveExclusions {
    root: PathBuf,
    configured: Vec<PathBuf>,
}

impl SaveExclusions {
    /// Rules for the project at `root` configured by `cfg`.
    pub(super) fn new(root: &Path, cfg: &CrapConfig) -> Self {
        let db = normalize(&cfg.db_path(root));

        let mut configured: Vec<PathBuf> = SQLITE_SIDECARS
            .iter()
            .map(|suffix| with_suffix(&db, suffix))
            .collect();
        configured.push(db);
        configured.push(normalize(&cfg.log_dir(root)));

        Self {
            root: normalize(root),
            configured,
        }
    }

    /// Whether `path` (an entry under the project root, not a symlink) stays
    /// out of the blueprint.
    pub(super) fn excludes(&self, path: &Path) -> bool {
        let path = normalize(path);
        let Some(name) = path.file_name() else {
            return false;
        };

        let at_top = path.parent() == Some(self.root.as_path());
        let name_str = name.to_string_lossy();

        if at_top && TOP_LEVEL_SKIP.contains(&name_str.as_ref()) {
            return true;
        }

        if self.configured.contains(&path)
            || is_staging_file_name(name)
            || name_str.starts_with(SECRET_PREFIX)
        {
            return true;
        }

        if path.is_dir() {
            return is_backup_dir(&path);
        }

        is_sqlite_file(&path) || is_sqlite_sidecar(&path, &name_str)
    }
}

/// `path` with `.` components removed, so configured and walked paths compare.
fn normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

/// `path` with `suffix` appended to its file name (`crap.db` → `crap.db-wal`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(suffix);

    PathBuf::from(raw)
}

/// A `backup` output directory: a manifest beside a database snapshot.
fn is_backup_dir(path: &Path) -> bool {
    path.join("manifest.json").is_file() && path.join("crap.db").is_file()
}

/// Whether the file starts with the `SQLite` 3 header.
fn is_sqlite_file(path: &Path) -> bool {
    let mut header = [0u8; 16];

    File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok_and(|()| &header == SQLITE_MAGIC)
}

/// A `-wal` / `-shm` / `-journal` file beside an `SQLite` database.
fn is_sqlite_sidecar(path: &Path, name: &str) -> bool {
    SQLITE_SIDECARS.iter().any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|base| is_sqlite_file(&path.with_file_name(base)))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn rules(root: &Path, toml: &str) -> SaveExclusions {
        let cfg: CrapConfig = toml::from_str(toml).unwrap();
        SaveExclusions::new(root, &cfg)
    }

    #[test]
    fn runtime_dirs_are_skipped_only_at_the_top_level() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ex = rules(root, "");

        for dir in ["data", "uploads", "types", "backups"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            assert!(ex.excludes(&root.join(dir)), "{dir}");
        }

        fs::create_dir_all(root.join("collections/data")).unwrap();
        assert!(!ex.excludes(&root.join("collections/data")));
    }

    /// Regression: a database configured outside `data/` (and its WAL
    /// sidecars) was copied into the blueprint.
    #[test]
    fn a_configured_database_outside_data_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ex = rules(root, "[database]\npath = \"./site.db\"\n");

        fs::write(root.join("site.db"), b"not yet a database").unwrap();
        fs::write(root.join("site.db-wal"), b"wal").unwrap();

        assert!(ex.excludes(&root.join("site.db")));
        assert!(ex.excludes(&root.join("site.db-wal")));
    }

    #[test]
    fn a_configured_log_dir_outside_data_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ex = rules(root, "[logging]\npath = \"logs\"\n");
        fs::create_dir_all(root.join("logs")).unwrap();

        assert!(ex.excludes(&root.join("logs")));
    }

    /// Regression: `backups/` (database snapshots plus the auth secret that
    /// unseals them) was copied into the blueprint — also a backup written to
    /// a custom `--output` inside the project.
    #[test]
    fn backup_directories_anywhere_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ex = rules(root, "");
        let custom = root.join("ops/snapshots/backup-2026");
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("manifest.json"), b"{}").unwrap();
        fs::write(custom.join("crap.db"), b"db").unwrap();

        assert!(ex.excludes(&custom));
        assert!(!ex.excludes(&root.join("ops")));
    }

    #[test]
    fn sqlite_files_secrets_and_staging_files_are_skipped_anywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ex = rules(root, "");
        let dir = root.join("misc");
        fs::create_dir_all(&dir).unwrap();

        let mut db = SQLITE_MAGIC.to_vec();
        db.extend_from_slice(b"rest");
        fs::write(dir.join("copy.bin"), &db).unwrap();
        fs::write(dir.join("copy.bin-shm"), b"shm").unwrap();
        fs::write(dir.join(".jwt_secret.pre-restore-1"), b"s").unwrap();
        fs::write(dir.join(".a.png.1.0.crap-tmp"), b"t").unwrap();
        fs::write(dir.join("notes.md"), b"keep").unwrap();

        assert!(ex.excludes(&dir.join("copy.bin")));
        assert!(ex.excludes(&dir.join("copy.bin-shm")));
        assert!(ex.excludes(&dir.join(".jwt_secret.pre-restore-1")));
        assert!(ex.excludes(&dir.join(".a.png.1.0.crap-tmp")));
        assert!(!ex.excludes(&dir.join("notes.md")));
    }
}
