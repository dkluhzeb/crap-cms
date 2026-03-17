# Installation

## Static Binary

Pre-built static binaries are attached to each [GitHub Release](https://github.com/dkluhzeb/crap-cms/releases). No runtime dependencies required.

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

## Docker

```bash
docker run -p 3000:3000 -p 50051:50051 \
  ghcr.io/dkluhzeb/crap-cms:latest serve /example
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
