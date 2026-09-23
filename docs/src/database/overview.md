# Database

Crap CMS supports two first-class database backends:

- **SQLite (default)** — zero configuration, single file, no server to manage, WAL mode for concurrent reads. The right choice for single-node deployments and the vast majority of workloads.
- **PostgreSQL** — enabled via `--features postgres` at build time. Full feature parity with SQLite: schema sync, migrations, full-text search (via `tsvector`), `_ref_count` delete protection, soft delete, atomic job claiming (`FOR UPDATE SKIP LOCKED`), and all query operators. No feature degradation — pick whichever matches your operational model. Works with PostgreSQL 12 or newer.

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

### Has-many list columns

A `has_many` text, number, select or radio field stores its values as a JSON
array in a `TEXT` column — its own column, one per locale when localized, a
group's `group__field` column, or an array table's column — and inside a
JSON-stored row (a blocks row, a group or array nested in a row) as a JSON
array too; so does the id list of a has-many relationship or upload inside an
array or blocks row. **Such a value is always a JSON array or NULL.** Every
write path and `crap-cms import` store it that way, and the schema sync repairs
a value a definition change left behind (see
[Changing a definition that has data](#changing-a-definition-that-has-data)).

Reads and filters rely on that and do not guard against anything else. If you
write rows around Crap CMS — raw SQL, an external ETL job — keep these columns
to JSON arrays (`'["news","tech"]'`, `'[1,2]'`) or NULL: a row holding any
other text makes every filter on that field **fail with a query error** rather
than silently match the wrong documents.

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
6. **Has-many lists** — a value of a has-many field that isn't stored as a list yet (the field was just switched to `has_many`, or its list retyped) is rewritten as one; see [Changing a definition that has data](#changing-a-definition-that-has-data)

Schema sync runs in a single transaction. If anything fails, all changes are rolled back. The one exception to "nothing outside the transaction" is a [soft-delete transition](../collections/soft-deletes.md#enabling-soft-deletes-on-an-existing-collection) on SQLite, which switches foreign-key enforcement off around that sync and verifies every reference before committing.

### Concurrent writers

On **SQLite** every write opens `BEGIN IMMEDIATE` — a database-wide write
lock — so writes are fully serialised and the per-row lock below is a
deliberate no-op there.

On **PostgreSQL** the same transaction is a plain `BEGIN` under MVCC, so two
writers of the same document run at once. The write path therefore locks the
document's row (`SELECT 1 … FOR UPDATE`) at the very start of the update —
before the pending draft, the stored row the access rules judge, the files
the write may drop and the outgoing-reference snapshot are read — and holds
it until commit. A write that changes only an array or blocks field takes
the lock too, even though it issues no `UPDATE`. Globals lock the `default`
row of their table the same way. Lock order is always the parent document
row first, then relationship targets.

## Changing a definition that has data

Schema sync only ever adds; what it does with each kind of change on a
table that already holds rows:

| Change | On the next boot |
|--------|------------------|
| Field added | Column added; existing rows read the field's `default_value` (applied at write time for new documents) or `null` |
| Field removed / renamed | Old column kept and reported as orphan (`db cleanup` drops it); a renamed field is a new, empty column |
| Field moved into a group | New `group__field` column; the old column is orphaned |
| Field `type` changed | **Boot refused** — migrate the column by hand (copy to a new field, or `ALTER` it yourself), then restart |
| `unique` added | A managed unique index is created; duplicates already present make the index creation fail — deduplicate first |
| `required` added | Validation only; no `NOT NULL` is retrofitted |
| `has_many` turned on (or a has-many list retyped) | Stored values are rewritten as lists once: a single value becomes a one-element list, a list's elements take the field's type, blank text becomes NULL. Text that isn't a JSON array is one value in a document's own column (`'Hello, world'` → `["Hello, world"]`) and comma-separated values inside an array or blocks row, where earlier releases stored a row's list that way (`'a,b'` → `["a","b"]`). A value holding nothing of the field's type (text in a `number` list) **refuses the boot**, naming the collection, column and document — fix or clear it, then restart. The same applies to a relationship or upload inside an array or blocks row turned `has_many` |
| `default_value` changed | Takes effect immediately — defaults are applied by the application |
| `localized` toggled | Values carried into the default locale's column and back (see the locale docs) |
| `soft_delete` enabled | Inline `UNIQUE` replaced by partial indexes (see soft deletes) |
| `soft_delete` disabled with trashed rows | **Boot refused** — purge the trash or re-enable |
| `versions` enabled | Version table created; existing documents get their first snapshot on their next write |
| Relationship target changed | Warned when the junction table holds rows — the old ids point at the old target |
| Collection or global removed | Tables kept; reported by the boot and by `db cleanup`, dropped only with `--drop-tables -y` |

## Connection Pool

On **SQLite** there are two pools (both r2d2): a **read pool**
(`pool_max_size`, default 64) and a small **write pool**
(`write_pool_max_size`, default 4). Writes take `BEGIN IMMEDIATE` and
serialize on SQLite's single writer, so excess writers queue on write-pool
checkout instead of consuming read connections and starving readers. On
**PostgreSQL** a single deadpool pool (`pool_max_size`) serves both;
`write_pool_max_size` is ignored. On both backends every checkout is bounded
by `[database] connection_timeout` — on Postgres it also bounds creating and
recycling a connection, and a connection the server closed (a restart, a
`pg_terminate_backend`) is dropped from the pool on its next checkout instead
of being handed out dead.

Each Postgres connection keeps a prepared-statement cache. A statement whose
plan a schema change invalidated (another node's schema sync, `crap-cms db
migrate`) is re-prepared once and the query retried in autocommit; inside a
transaction — which the failure has already aborted — the statement is
evicted and the error reported, and the next transaction re-prepares it.

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
