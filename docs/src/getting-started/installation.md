# Installation

## Prerequisites

- **Rust** (edition 2021) — install via [rustup](https://rustup.rs/)
- **SQLite** development libraries (usually pre-installed on macOS/Linux)
- **C compiler** — required by rusqlite and image processing dependencies

On Debian/Ubuntu:

```bash
sudo apt install build-essential libsqlite3-dev
```

On macOS (with Homebrew):

```bash
brew install sqlite3
```

On Arch Linux:

```bash
sudo pacman -S sqlite
```

## Building

Clone the repository and build:

```bash
git clone https://github.com/your-org/crap-cms.git
cd crap-cms
cargo build --release
```

The binary is at `target/release/crap-cms`.

## Optional Tools

- **grpcurl** — for testing the gRPC API from the command line. See [grpcurl installation](https://github.com/fullstorydev/grpcurl#installation).
- **lua-language-server** (LuaLS) — for IDE autocompletion in Lua config files. The project provides type definitions in `types/crap.lua`.
