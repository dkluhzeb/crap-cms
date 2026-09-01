# Database

Crap CMS supports two first-class database backends:

- **SQLite (default)** — zero configuration, single file, no server to manage, WAL mode for concurrent reads. The right choice for single-node deployments and the vast majority of workloads.
- **PostgreSQL** — enabled via `--features postgres` at build time. Full feature parity with SQLite: schema sync, migrations, full-text search (via `tsvector`), `_ref_count` delete protection, soft delete, atomic job claiming (`FOR UPDATE SKIP LOCKED`), and all query operators. No feature degradation — pick whichever matches your operational model.

The choice primarily comes down to your deployment topology:

- Single server, single writer → **SQLite** (simpler, one binary, one file to back up).
- Multi-server / high availability / dedicated job workers → **PostgreSQL** (shared writer across nodes).

See [Multi-Server Deployment](../deployment/multi-server.md) for the full multi-node setup. The rest of this page documents the schema conventions and sync behavior that apply to both backends; SQLite is used for the examples.

## Configuration

```toml
[database]
path = "data/crap.db"       # relative to config dir, or absolute
pool_max_size = 64           # READ pool size
write_pool_max_size = 4      # WRITE pool size (SQLite only)
cache_size = -16384          # page cache in KB (16MB)
mmap_size = 268435456        # memory-mapped I/O (256MB)
```

For PostgreSQL:

```toml
[database]
backend = "postgres"
url = "host=db.example.com user=crap dbname=crap_cms"
```

## WAL Mode (SQLite)

The database runs in WAL (Write-Ahead Logging) mode for better concurrent read performance. This is set automatically on every pooled connection, along with `synchronous = NORMAL`, `foreign_keys = ON`, `temp_store = MEMORY`, and the configurable `busy_timeout`, `wal_autocheckpoint`, `cache_size`, and `mmap_size` pragmas from `[database]`.

## Schema

### Collection Tables

Each collection gets a table named after its slug:

```sql
CREATE TABLE posts (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    status TEXT DEFAULT 'draft',
    content TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

Column types are determined by field types:

| Field Type | SQLite Type | PostgreSQL Type |
|-----------|-------------|-----------------|
| text, textarea, richtext, select, date, email, json | TEXT | TEXT |
| number | REAL | DOUBLE PRECISION |
| checkbox | INTEGER | SMALLINT |
| relationship (has-one) | TEXT | TEXT |

Auth collections also get a `_password_hash TEXT` column.

### Global Tables

Named `_global_{slug}`, always have a single row with `id = 'default'`:

```sql
CREATE TABLE _global_site_settings (
    id TEXT PRIMARY KEY,
    site_name TEXT,
    tagline TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

### Junction Tables

Has-many relationships and arrays use join tables:

```sql
-- Has-many relationship: posts_tags
CREATE TABLE posts_tags (
    parent_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    related_id TEXT NOT NULL,
    _order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (parent_id, related_id)
);

-- Array field: posts_slides
CREATE TABLE posts_slides (
    id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    _order INTEGER NOT NULL DEFAULT 0,
    title TEXT,
    image_url TEXT,
    caption TEXT
);
```

### Metadata Table

```sql
CREATE TABLE _crap_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT DEFAULT (datetime('now'))
);
```

## Dynamic Schema Sync

On startup, Crap CMS compares Lua definitions against the database schema:

1. **Missing tables** — created with all columns
2. **Missing columns** — added via `ALTER TABLE ADD COLUMN`
3. **Missing junction tables** — created for new has-many/array fields
4. **Removed columns** — logged as warnings (not dropped)
5. **Missing `_password_hash`** — added to auth collections

Schema sync runs in a single transaction. If anything fails, all changes are rolled back.

## Connection Pool

On **SQLite** there are two pools (both r2d2): a **read pool**
(`pool_max_size`, default 64) and a small **write pool**
(`write_pool_max_size`, default 4). Writes take `BEGIN IMMEDIATE` and
serialize on SQLite's single writer, so excess writers queue on write-pool
checkout instead of consuming read connections and starving readers. On
**PostgreSQL** a single deadpool pool (`pool_max_size`) serves both;
`write_pool_max_size` is ignored.

- **Read operations** — `db/ops.rs` gets a connection from the read pool, calls `query::*` functions
- **Write operations** — callers get a connection from the write pool, open a transaction, call `query::*`, then commit
- **Hook CRUD** — hooks share the caller's transaction via the TxContext pattern

## Transaction Pattern

All write operations follow this pattern:

```
1. Get connection from pool
2. Begin transaction
3. Run before-hooks (with transaction access)
4. Execute query (inside same transaction)
5. Run after-hooks (inside same transaction, errors roll back)
6. Commit transaction
```
