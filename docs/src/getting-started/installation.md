# Installation

## Quick Install (Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash
```

The installer auto-detects your architecture (x86_64 or aarch64), downloads the matching release, verifies its SHA256, and lays out a version store under `~/.local/share/crap-cms/`:

```
~/.local/share/crap-cms/
├── versions/
│   └── v0.1.0-alpha.5/crap-cms
└── current -> versions/v0.1.0-alpha.5/crap-cms

~/.local/bin/crap-cms -> ~/.local/share/crap-cms/current
```

After install, make sure `~/.local/bin` is on your `PATH` — the installer prints the exact `export PATH=…` line to add to `~/.bashrc` / `~/.zshrc` if it isn't.

### Verify before running

If you don't want to pipe straight into `bash`:

```bash
curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh -o install.sh
less install.sh                  # audit the script
sha256sum install.sh             # compare against scripts/install.sh in the repo
bash install.sh                  # run once satisfied
```

Pin to a specific tag for a reproducible install:

```bash
curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/v0.1.0-alpha.5/scripts/install.sh | bash
```

Override the locations via environment variables:

```bash
XDG_DATA_HOME=/opt/crap-cms BIN_DIR=/usr/local/bin \
  curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash
```

## Managing Versions

Once the binary is installed via the script, `crap-cms update` acts as a built-in version manager — similar to `rustup` or `nvm`:

| Command | What it does |
|---------|--------------|
| `crap-cms update check` | Compare the running version to the latest GitHub release. Exit 0 if up-to-date, 1 otherwise. Caches the result for 24h. |
| `crap-cms update list` | List all remote release tags. Installed versions are marked `(installed)`; the active one is marked `*`. |
| `crap-cms update install <version>` | Download + verify + stage a specific version in the store (does not switch). |
| `crap-cms update use <version>` | Switch the `current` symlink to an installed version. Atomic. |
| `crap-cms update uninstall <version>` | Remove an installed version. Refuses the active one. |
| `crap-cms update where` | Print the resolved path of the currently active binary. |
| `crap-cms update` | Install latest + switch to it. Prompts for confirmation (skip with `-y`). |

On `crap-cms serve` startup, a one-line notice prints when a newer release is in the cache. Silence it via `crap.toml`:

```toml
[update]
check_on_startup = false
```

**Distro-managed installs refuse self-update.** If the running binary lives under `/usr/`, `/opt/`, or `/nix/`, `crap-cms update` refuses with a message telling you to update via your package manager. Use `--force` to bypass.

**Windows.** `crap-cms update install` / `use` / the bare `crap-cms update` are not supported on Windows yet — the version-store layout uses symlinks, which require Developer Mode or admin privileges on Windows. The read-only subcommands (`check`, `list`, `where`) still work. Windows users should download `crap-cms-windows-x86_64.exe` manually from the [releases page](https://github.com/dkluhzeb/crap-cms/releases/latest) and replace their binary to upgrade.

## Direct Download

Prefer to install manually? Grab a binary straight from [GitHub Releases](https://github.com/dkluhzeb/crap-cms/releases). No runtime dependencies:

```bash
curl -L -o crap-cms \
  https://github.com/dkluhzeb/crap-cms/releases/latest/download/crap-cms-linux-x86_64
chmod +x crap-cms
sudo mv crap-cms /usr/local/bin/
```

Available binaries:

| File | Platform |
|------|----------|
| `crap-cms-linux-x86_64` | Linux x86_64 (musl, fully static) |
| `crap-cms-linux-aarch64` | Linux ARM64 (musl, fully static) |
| `crap-cms-windows-x86_64.exe` | Windows x86_64 |

Binaries installed to `/usr/local/bin` this way cannot be self-updated via `crap-cms update` (same rule as distro packages). Use the quick installer if you want version management.

## Docker

```bash
docker run -p 3000:3000 -p 50051:50051 \
  ghcr.io/dkluhzeb/crap-cms:latest serve -C /example
```

Images are Alpine-based (~30 MB) and published to `ghcr.io/dkluhzeb/crap-cms`. See the [README](https://github.com/dkluhzeb/crap-cms#docker) for production usage and available tags.

## Building from Source

Requires a Rust toolchain (edition 2024) via [rustup](https://rustup.rs/) and a C compiler:

```bash
git clone https://github.com/dkluhzeb/crap-cms.git
cd crap-cms
cargo build --release
```

The binary is at `target/release/crap-cms`. SQLite and Lua are bundled — no system libraries needed.

## Optional Tools

- **grpcurl** — for testing the gRPC API from the command line. See [grpcurl installation](https://github.com/fullstorydev/grpcurl#installation).
- **lua-language-server** (LuaLS) — for IDE autocompletion in Lua config files. The project provides type definitions in `types/crap.lua`.
