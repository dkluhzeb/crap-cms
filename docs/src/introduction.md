# Crap CMS

Crap CMS is a headless content management system built in Rust. It combines a compiled core with Lua hooks (neovim-style) and an overridable HTMX admin UI.

## Motivation

I built several Rust/WebAssembly frontend projects and couldn't find a CMS that fit the stack. So I built one.

The idea: a simple CMS written in Rust, extensible via a lightweight scripting language, with no complicated build steps or infrastructure requirements. It's also a playground for me to explore ideas and learn — which means things may change, break, or get rewritten.

Inspiration came from what I consider the best solutions out there:

- **Lua scripting API** — modeled after Neovim and Awesome WM, where Lua gives users deep control without touching core code
- **Configuration & hook system** — inspired by [Payload CMS](https://payloadcms.com), an excellent and highly recommended CMS for anyone needing a production-ready solution
- **CLI tooling** — influenced by Laravel's comprehensive Artisan CLI
- **SQLite + WAL + FTS** — sufficient for most of my use cases, and it bundles cleanly into a single binary with zero external dependencies. The database layer is abstracted behind a trait, so alternative relational backends could be added in the future if the need arises
- **Pure JavaScript with JSDoc types** — no TypeScript, no bundler, no build step. Type safety through JSDoc annotations, checkable with `tsc --checkJs` without compiling anything
- **HTMX + Web Components** — easy to theme (similar to WordPress child themes), no frontend build step. Web Components are a native browser standard — no framework updates, no outdated dependencies, no breaking changes
- **gRPC API** — binary protocol with streaming support, ideal for service-to-service communication. And because I wanted it. A separate [REST proxy](https://github.com/dkluhzeb/crap-rest) is available for those who prefer plain JSON over HTTP

The project is functional but not yet production-ready — it still needs to prove itself.

**Warning:** While in alpha (`0.x`), breaking changes may appear without prior notice.

## Design Philosophy

- **Lua is the single source of truth for schemas.** Collections and fields are defined in Lua files, not in the database. The database stores content, not structure.
- **Single binary.** The admin UI (Axum) and gRPC API (Tonic) run in one process on two ports.
- **Config directory pattern.** All customization lives in one directory, passed as a positional argument to each command.
- **No JS build step.** The admin UI uses Handlebars templates + HTMX with plain CSS.
- **Hooks are the extension system.** Lua hooks at three levels (field, collection, global) provide full lifecycle control with transaction-safe CRUD access.

## Feature Set

- **Collections** with 14 field types (text, number, textarea, richtext, select, checkbox, date, email, json, relationship, array, group, upload, blocks)
- **Globals** — single-document collections for site-wide settings
- **Hooks** — field-level, collection-level, and globally registered lifecycle hooks
- **Access Control** — collection-level and field-level, with filter constraint support
- **Authentication** — JWT sessions, password login, custom auth strategies, email verification, password reset
- **Uploads** — file uploads with automatic image resizing and format conversion (WebP, AVIF)
- **Relationships** — has-one and has-many with configurable population depth
- **Localization** — per-field opt-in localization with locale-suffixed columns
- **Versions & Drafts** — document version history with draft/publish workflow
- **Live Updates** — real-time mutation events via SSE and gRPC streaming
- **Admin UI** — template overlay system, theme switching, Web Components
- **gRPC API** — full CRUD with filtering, pagination, and server reflection
- **CLI Tooling** — interactive scaffolding wizard, blueprints, data export/import, backups

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2024) |
| Web framework | Axum |
| gRPC | Tonic + Prost |
| Database | SQLite via rusqlite, r2d2 pool, WAL mode |
| Templates | Handlebars + HTMX |
| Hooks | Lua 5.4 via mlua |
| IDs | nanoid |
