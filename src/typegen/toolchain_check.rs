//! Compile checks of generated client types with each language's own
//! toolchain, when one is installed: `deno check` for TypeScript, `go vet`
//! for Go, and for Python an import that instantiates every dataclass and
//! resolves its annotations. Parsing (as the Rust checks do with `syn`)
//! misses what only a compiler sees — a duplicate or shadowed declaration, a
//! reference to a type the file does not declare — so these run the real
//! thing. Each returns `None` when its toolchain is missing, and the caller
//! skips with a note instead of failing the hermetic suite.

use std::{env, fs, path::PathBuf, process::Command};

use tempfile::TempDir;

/// The module file `go vet` builds the generated package in: the oldest Go
/// with the generics the generated `Rel[T]` needs.
const GO_MOD: &str = "module generated\n\ngo 1.18\n";

/// Imports the generated Python module, then instantiates every dataclass
/// it declares and resolves that class's annotations — the step where a
/// class shadowing a `typing` name, or an annotation naming a class the file
/// lacks, fails. The generated `list[str]` annotations need Python 3.9.
const PY_CHECK: &str = r#"import dataclasses, importlib.util, sys, typing

if sys.version_info < (3, 9):
    print("skipping: Python 3.9+ needed")
    sys.exit(0)

spec = importlib.util.spec_from_file_location("generated", "generated.py")
module = importlib.util.module_from_spec(spec)
sys.modules["generated"] = module
spec.loader.exec_module(module)

for value in list(vars(module).values()):
    if dataclasses.is_dataclass(value) and value.__module__ == "generated":
        typing.get_type_hints(value)
        value()
"#;

/// The executable `name` found on `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;

    env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// `files` written into a fresh scratch directory.
fn scratch(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a toolchain scratch dir");

    for (name, content) in files {
        fs::write(dir.path().join(name), content).expect("write a toolchain scratch file");
    }

    dir
}

/// Run `command` in `dir`: `Ok` when it exits 0, else its output.
fn outcome(command: &mut Command, dir: &TempDir) -> Result<(), String> {
    let output = command
        .current_dir(dir.path())
        .output()
        .map_err(|e| format!("cannot run {command:?}: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// `deno check` of the TypeScript `files` (a file may import another as
/// `./name.ts`), offline. `None` without `deno` on `PATH`.
pub(super) fn typescript(files: &[(&str, &str)]) -> Option<Result<(), String>> {
    let deno = on_path("deno")?;
    let dir = scratch(files);

    let mut command = Command::new(deno);
    command
        .args(["check", "--no-remote", "--quiet"])
        .args(files.iter().map(|(name, _)| *name))
        .env("DENO_NO_UPDATE_CHECK", "1");

    Some(outcome(&mut command, &dir))
}

/// `go vet` of the Go `source` as a package of its own, with no toolchain
/// download. `None` without `go` on `PATH`.
pub(super) fn go(source: &str) -> Option<Result<(), String>> {
    let go = on_path("go")?;
    let dir = scratch(&[("go.mod", GO_MOD), ("types.go", source)]);

    let mut command = Command::new(go);
    command
        .args(["vet", "./..."])
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod");

    Some(outcome(&mut command, &dir))
}

/// Import the Python `source` and check every dataclass it declares (see
/// [`PY_CHECK`]). `None` without `python3` on `PATH`.
pub(super) fn python(source: &str) -> Option<Result<(), String>> {
    let python = on_path("python3")?;
    let dir = scratch(&[("generated.py", source), ("check.py", PY_CHECK)]);

    let mut command = Command::new(python);
    command.arg("check.py");

    Some(outcome(&mut command, &dir))
}
