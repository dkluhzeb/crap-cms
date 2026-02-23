# Crap CMS

Crap CMS is a headless content management system built in Rust. It combines a compiled core with Lua hooks (neovim-style) and an overridable HTMX admin UI.

## Design Philosophy

- **Lua is the single source of truth for schemas.** Collections and fields are defined in Lua files, not in the database. The database stores content, not structure.
- **Single binary.** The admin UI (Axum) and gRPC API (Tonic) run in one process on two ports.
- **Config directory pattern.** All customization lives in one directory passed via `--config`.
- **No JS build step.** The admin UI uses Handlebars templates + HTMX with plain CSS.
- **Hooks are the extension system.** Lua hooks at three levels (field, collection, global) provide full lifecycle control with transaction-safe CRUD access.

## Feature Set

Crap CMS targets PayloadCMS feature parity:

- **Collections** with 11 field types (text, number, textarea, richtext, select, checkbox, date, email, json, relationship, array)
- **Globals** — single-document collections for site-wide settings
- **Hooks** — field-level, collection-level, and globally registered lifecycle hooks
- **Access Control** — collection-level and field-level, with filter constraint support
- **Authentication** — JWT sessions, password login, custom auth strategies
- **Uploads** — file uploads with automatic image resizing and format conversion (WebP, AVIF)
- **Relationships** — has-one and has-many with configurable population depth
- **Admin UI** — template overlay system, theme switching, Web Components
- **gRPC API** — full CRUD with filtering, pagination, and server reflection

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2021) |
| Web framework | Axum |
| gRPC | Tonic + Prost |
| Database | SQLite via rusqlite, r2d2 pool, WAL mode |
| Templates | Handlebars + HTMX |
| Hooks | Lua 5.4 via mlua |
| IDs | nanoid |
