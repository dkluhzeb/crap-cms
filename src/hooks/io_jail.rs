//! Path jail for the Lua `io` library.
//!
//! Hook code keeps `io` file access (custom storage backends map to the
//! filesystem), but only below the allowed roots: the config directory plus
//! the operator's `[hooks] io_roots`. Inside them the process's own secrets
//! stay out of reach — the config file, the data directory (generated auth
//! secret, `SQLite` database, PID files), backups, logs and the database
//! file wherever it lives — and so do the kernel's process views (`/proc`,
//! `/sys`, `/dev`), which would otherwise hand out the environment
//! (`CRAP_SECRET_*` included) regardless of any root.
//!
//! Every path-taking `io` function (`open`, `lines`, `input`, `output`) is
//! wrapped: the path is resolved to its canonical form (symlinks followed;
//! for a file that does not exist yet, its deepest existing ancestor), the
//! resolved path is checked, and the original function then opens exactly
//! that resolved path.

use std::{
    env, fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    result::Result as StdResult,
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, bail};
use mlua::{Function, Lua, String as LuaString, Table};

use crate::config::CrapConfig;

/// The `io` functions whose first argument may be a file path.
const PATH_FUNCTIONS: [&str; 4] = ["open", "lines", "input", "output"];

/// Kernel views that expose the process (its environment, memory, open
/// files) — refused under any root.
#[cfg(unix)]
const SYSTEM_VIEWS: [&str; 3] = ["/proc", "/sys", "/dev"];
#[cfg(not(unix))]
const SYSTEM_VIEWS: [&str; 0] = [];

/// Whether the platform's default filesystems ignore letter case (APFS and
/// NTFS do). There a path that does not exist yet keeps the caller's
/// spelling through resolution — `CRAP.DB-JOURNAL` would create the
/// database's `crap.db-journal` — so protected paths are matched without
/// regard to case.
const FOLD_CASE: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Wraps one `io` function: a string (or number, which Lua's `io` coerces)
/// path goes through `resolve`; anything else (`nil` = default file, a file
/// handle) passes through untouched. A refusal is raised at the caller's
/// line.
const WRAPPER: &str = r#"
local original, resolve, name = ...

return function(path, ...)
    if type(path) == "number" then
        path = tostring(path)
    end

    if type(path) == "string" then
        local resolved, err = resolve(path)

        if not resolved then
            error(name .. ": " .. err, 2)
        end

        path = resolved
    end

    return original(path, ...)
end
"#;

/// The resolved allow/deny sets `io` paths are checked against. Built once
/// per process from the config and shared by every VM.
#[derive(Debug)]
pub(crate) struct IoJail {
    /// Canonical roots a path must lie under.
    roots: Vec<PathBuf>,
    /// Paths refused even under a root (and everything below them).
    protected: Vec<PathBuf>,
}

impl IoJail {
    /// Resolve the config directory and `[hooks] io_roots` into the jail.
    ///
    /// # Errors
    ///
    /// An `io_roots` entry that does not exist, is not a directory, is a
    /// filesystem root, or lies inside a kernel view (`/proc`, `/sys`,
    /// `/dev`) or a protected path.
    pub(crate) fn new(config_dir: &Path, config: &CrapConfig) -> Result<Self> {
        let config_root = resolve_path(config_dir)
            .map_err(|e| anyhow!("config directory {}: {e}", config_dir.display()))?;

        let protected = protected_paths(&config_root, config);
        let mut roots = vec![config_root.clone()];

        for entry in &config.hooks.io_roots {
            let root = extra_root(&config_root, entry)?;

            // Every path under such a root would be refused anyway — a
            // configuration that cannot do what it says.
            if protected.iter().any(|p| lies_under(&root, p, FOLD_CASE)) {
                bail!(
                    "hooks.io_roots entry '{entry}' lies inside a protected path \
                     (data, backups, logs, database)"
                );
            }

            roots.push(root);
        }

        Ok(Self { roots, protected })
    }

    /// The canonical path `raw` may be opened as, or why it is refused.
    fn resolve(&self, raw: &str) -> StdResult<String, String> {
        if raw.contains('\0') {
            return Err("the path contains a NUL byte".to_string());
        }

        let path = resolve_path(Path::new(raw)).map_err(|e| format!("'{raw}' {e}"))?;

        if self
            .protected
            .iter()
            .any(|p| lies_under(&path, p, FOLD_CASE))
        {
            return Err(format!(
                "'{raw}' is a protected path — the config file, data directory, database, \
                 backups, logs and system views are never reachable from Lua"
            ));
        }

        if !self.roots.iter().any(|r| path.starts_with(r)) {
            return Err(format!(
                "'{raw}' is outside the allowed roots (the config directory and \
                 `[hooks] io_roots` in crap.toml)"
            ));
        }

        path.into_os_string()
            .into_string()
            .map_err(|_| format!("'{raw}' resolves to a non-UTF-8 path"))
    }
}

/// Whether `path` is `prefix` or lies below it, comparing components
/// case-insensitively when `fold_case` is set.
fn lies_under(path: &Path, prefix: &Path, fold_case: bool) -> bool {
    if !fold_case {
        return path.starts_with(prefix);
    }

    let mut components = path.components();

    prefix.components().all(|want| {
        components.next().is_some_and(|got| {
            got.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&want.as_os_str().to_string_lossy())
        })
    })
}

/// One `[hooks] io_roots` entry, relative to the config directory unless
/// absolute, resolved to its canonical form.
fn extra_root(config_root: &Path, entry: &str) -> Result<PathBuf> {
    let path = config_root.join(entry);

    let root = fs::canonicalize(&path)
        .with_context(|| format!("hooks.io_roots entry '{entry}' cannot be resolved"))?;

    if !root.is_dir() {
        bail!("hooks.io_roots entry '{entry}' is not a directory");
    }

    if root.parent().is_none() {
        bail!("hooks.io_roots entry '{entry}' is a filesystem root");
    }

    if SYSTEM_VIEWS.iter().any(|v| root.starts_with(v)) {
        bail!("hooks.io_roots entry '{entry}' lies inside a system view (/proc, /sys, /dev)");
    }

    Ok(root)
}

/// Paths refused under every root, each both as configured and resolved
/// (a symlinked data directory is refused under either name).
fn protected_paths(config_root: &Path, config: &CrapConfig) -> Vec<PathBuf> {
    let db = config.db_path(config_root);
    let sidecar = |suffix: &str| {
        let mut name = db.clone().into_os_string();
        name.push(suffix);
        PathBuf::from(name)
    };

    let mut listed = vec![
        config_root.join("crap.toml"),
        config_root.join("data"),
        config_root.join("backups"),
        config.log_dir(config_root),
        sidecar("-wal"),
        sidecar("-shm"),
        sidecar("-journal"),
        db,
    ];
    listed.extend(SYSTEM_VIEWS.iter().map(PathBuf::from));

    let resolved: Vec<PathBuf> = listed.iter().filter_map(|p| resolve_path(p).ok()).collect();
    listed.extend(resolved);

    listed
}

/// The canonical absolute form of `path` (relative paths resolve against
/// the working directory, as `fopen` does). A path that does not exist yet
/// resolves through its deepest existing ancestor.
fn resolve_path(path: &Path) -> StdResult<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|e| format!("cannot be resolved: {e}"))?
            .join(path)
    };

    match fs::canonicalize(&absolute) {
        Ok(canonical) => Ok(canonical),
        Err(e) if e.kind() == ErrorKind::NotFound => resolve_missing(&absolute),
        Err(e) => Err(format!("cannot be resolved: {e}")),
    }
}

/// Resolve a path that does not exist: canonicalize its deepest existing
/// ancestor and append the missing tail.
fn resolve_missing(absolute: &Path) -> StdResult<PathBuf, String> {
    for ancestor in absolute.ancestors().skip(1) {
        let base = match fs::canonicalize(ancestor) {
            Ok(base) => base,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot be resolved: {e}")),
        };

        let tail = absolute
            .strip_prefix(ancestor)
            .map_err(|_| "cannot be resolved".to_string())?;

        return join_missing_tail(ancestor, &base, tail);
    }

    Err("cannot be resolved".to_string())
}

/// Append the missing `tail` to the canonical `base`. The tail must be
/// plain names (a `..` after a missing directory cannot be resolved), and
/// its first component must not be a dangling symbolic link — opening it
/// for writing would create the link's target, wherever it points.
fn join_missing_tail(ancestor: &Path, base: &Path, tail: &Path) -> StdResult<PathBuf, String> {
    if !tail.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("cannot be resolved (it walks through a missing directory)".to_string());
    }

    let Some(Component::Normal(first)) = tail.components().next() else {
        return Ok(base.to_path_buf());
    };

    if fs::symlink_metadata(ancestor.join(first)).is_ok() {
        return Err("is a dangling symbolic link".to_string());
    }

    Ok(base.join(tail))
}

/// Wrap every path-taking `io` function of `lua` in the jail.
///
/// # Errors
///
/// Returns an error if the `io` table or one of its functions is missing,
/// or a wrapper can't be built.
pub(crate) fn install_io_jail(lua: &Lua, jail: &Arc<IoJail>) -> Result<()> {
    let io: Table = lua.globals().get("io")?;

    let jail = Arc::clone(jail);
    let resolve = lua.create_function(move |_, raw: LuaString| {
        let resolved = match raw.to_str() {
            Ok(raw) => jail.resolve(&raw),
            Err(_) => Err("the path is not valid UTF-8".to_string()),
        };

        Ok(match resolved {
            Ok(path) => (Some(path), None),
            Err(reason) => (None, Some(reason)),
        })
    })?;

    for name in PATH_FUNCTIONS {
        let original: Function = io.get(name)?;

        let wrapped: Function = lua
            .load(WRAPPER)
            .set_name("=io_jail")
            .call((original, resolve.clone(), format!("io.{name}")))
            .with_context(|| format!("Failed to wrap io.{name}"))?;

        io.set(name, wrapped)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use mlua::{LuaOptions, StdLib, Value};

    use super::*;

    /// A sandboxed-io VM jailed to `config_dir` (plus `io_roots`).
    fn jailed_lua(config_dir: &Path, io_roots: &[&str]) -> Lua {
        let mut config = CrapConfig::default();
        config.hooks.io_roots = io_roots.iter().map(ToString::to_string).collect();

        let jail = Arc::new(IoJail::new(config_dir, &config).unwrap());
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        install_io_jail(&lua, &jail).unwrap();

        lua
    }

    /// Run `code` with the Lua global `path` set, returning the error text.
    fn refused(lua: &Lua, code: &str, path: &Path) -> String {
        set_path(lua, path);

        lua.load(code)
            .exec()
            .expect_err("the jail must refuse this path")
            .to_string()
    }

    fn set_path(lua: &Lua, path: &Path) {
        lua.globals()
            .set("path", path.to_string_lossy().to_string())
            .unwrap();
    }

    #[test]
    fn reads_and_writes_under_the_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("uploads/sub")).unwrap();
        let lua = jailed_lua(tmp.path(), &[]);
        set_path(&lua, &tmp.path().join("uploads/sub/new.txt"));

        let read: String = lua
            .load(
                r#"
                local f = assert(io.open(path, "w"))
                f:write("hello")
                f:close()

                local lines = {}
                for line in io.lines(path) do
                    lines[#lines + 1] = line
                end

                return table.concat(lines)
                "#,
            )
            .eval()
            .unwrap();

        assert_eq!(read, "hello");
        assert_eq!(
            fs::read_to_string(tmp.path().join("uploads/sub/new.txt")).unwrap(),
            "hello"
        );
    }

    /// A missing file under a root keeps `io.open`'s `nil, message`
    /// convention — only a policy refusal raises.
    #[test]
    fn a_missing_file_under_a_root_returns_nil() {
        let tmp = tempfile::tempdir().unwrap();
        let lua = jailed_lua(tmp.path(), &[]);
        set_path(&lua, &tmp.path().join("absent.txt"));

        let (file, err): (Value, String) = lua.load("return io.open(path)").eval().unwrap();

        assert!(matches!(file, Value::Nil));
        assert!(err.contains("absent.txt"), "{err}");
    }

    /// The kernel's view of the process hands out every environment variable
    /// (`CRAP_SECRET_*` included) — refused under any configuration.
    #[cfg(target_os = "linux")]
    #[test]
    fn refuses_proc_self_environ() {
        let tmp = tempfile::tempdir().unwrap();
        let lua = jailed_lua(tmp.path(), &[]);

        let err = refused(&lua, "io.open(path)", Path::new("/proc/self/environ"));

        assert!(err.contains("protected path"), "{err}");
    }

    /// The data directory (generated secret, database), backups, and the
    /// config file sit under the config dir but stay unreachable.
    #[test]
    fn refuses_the_secrets_under_the_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("data")).unwrap();
        fs::create_dir_all(tmp.path().join("backups")).unwrap();
        fs::write(tmp.path().join("data/.jwt_secret"), "s3cret").unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let lua = jailed_lua(tmp.path(), &[]);

        for target in [
            "data/.jwt_secret",
            "data/crap.db",
            "data/crap.db-wal",
            "backups/new.tar",
            "crap.toml",
        ] {
            let err = refused(&lua, "io.open(path)", &tmp.path().join(target));

            assert!(err.contains("protected path"), "{target}: {err}");
        }

        let err = refused(
            &lua,
            "io.output(path)",
            &tmp.path().join("data/.jwt_secret"),
        );
        assert!(err.contains("io.output"), "{err}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("data/.jwt_secret")).unwrap(),
            "s3cret",
            "a refused write must not truncate the file"
        );
    }

    #[test]
    fn refuses_a_parent_dir_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(tmp.path().join("outside.txt"), "x").unwrap();
        let lua = jailed_lua(&config, &[]);

        for code in ["io.open(path)", "io.lines(path)", "io.input(path)"] {
            let err = refused(&lua, code, &config.join("../outside.txt"));

            assert!(err.contains("outside the allowed roots"), "{code}: {err}");
        }

        let err = refused(&lua, "io.open(path, 'w')", &config.join("../new.txt"));
        assert!(err.contains("outside the allowed roots"), "{err}");
        assert!(!tmp.path().join("new.txt").exists());
    }

    #[test]
    fn refuses_an_absolute_path_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("f.txt"), "x").unwrap();
        let lua = jailed_lua(tmp.path(), &[]);

        let err = refused(&lua, "io.open(path)", &other.path().join("f.txt"));

        assert!(err.contains("outside the allowed roots"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_escapes_the_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "x").unwrap();
        symlink(&outside, config.join("link")).unwrap();
        symlink(outside.join("missing.txt"), config.join("dangling")).unwrap();
        let lua = jailed_lua(&config, &[]);

        let err = refused(&lua, "io.open(path)", &config.join("link/secret.txt"));
        assert!(err.contains("outside the allowed roots"), "{err}");

        let err = refused(&lua, "io.open(path, 'w')", &config.join("link/new.txt"));
        assert!(err.contains("outside the allowed roots"), "{err}");

        let err = refused(&lua, "io.open(path, 'w')", &config.join("dangling"));
        assert!(err.contains("dangling symbolic link"), "{err}");
        assert!(!outside.join("missing.txt").exists());
    }

    #[test]
    fn an_extra_root_is_reachable() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tempfile::tempdir().unwrap();
        fs::write(media.path().join("a.txt"), "media").unwrap();
        let root = media.path().to_string_lossy().to_string();
        let lua = jailed_lua(tmp.path(), &[&root]);
        set_path(&lua, &media.path().join("a.txt"));

        let read: String = lua
            .load("local f = assert(io.open(path)); local s = f:read('a'); f:close(); return s")
            .eval()
            .unwrap();

        assert_eq!(read, "media");
    }

    #[test]
    fn io_roots_entries_are_checked_at_build() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file.txt");
        fs::write(&file, "x").unwrap();

        let build = |entry: &str| {
            let mut config = CrapConfig::default();
            config.hooks.io_roots = vec![entry.to_string()];
            IoJail::new(tmp.path(), &config).unwrap_err().to_string()
        };

        assert!(build("missing-dir").contains("cannot be resolved"));
        assert!(build("file.txt").contains("not a directory"));
        assert!(build("/").contains("filesystem root"));

        fs::create_dir_all(tmp.path().join("data/media")).unwrap();
        assert!(build("data/media").contains("protected path"));

        #[cfg(target_os = "linux")]
        assert!(build("/proc/self").contains("system view"));
    }

    /// On a case-insensitive filesystem a not-yet-existing path keeps the
    /// caller's spelling, so a protected path must match regardless of case
    /// there — and only by whole components.
    #[test]
    fn lies_under_folds_case_only_when_asked() {
        let db = Path::new("/srv/site/crap.db-journal");

        assert!(lies_under(Path::new("/srv/site/CRAP.DB-JOURNAL"), db, true));
        assert!(!lies_under(
            Path::new("/srv/site/CRAP.DB-JOURNAL"),
            db,
            false
        ));
        assert!(lies_under(
            Path::new("/srv/Site/DATA/x"),
            Path::new("/srv/site/data"),
            true
        ));
        assert!(!lies_under(
            Path::new("/srv/site/database"),
            Path::new("/srv/site/data"),
            true
        ));
        assert!(!lies_under(
            Path::new("/srv/site"),
            Path::new("/srv/site/data"),
            true
        ));
    }

    /// Default files and handles pass through the wrappers untouched.
    #[test]
    fn non_path_arguments_pass_through() {
        let tmp = tempfile::tempdir().unwrap();
        let lua = jailed_lua(tmp.path(), &[]);

        let same: bool = lua
            .load("return io.output() == io.output(io.output())")
            .eval()
            .unwrap();

        assert!(same);
    }
}
