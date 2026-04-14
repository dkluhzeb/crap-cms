# Cache Backend

Crap CMS uses a pluggable cache backend for cross-request caching of populated relationship documents. The cache is cleared automatically on any write operation (create, update, delete) and optionally on a periodic timer.

## Backends

### Memory (default)

In-process `DashMap` with a configurable soft entry cap. Fast, zero infrastructure, but per-server — each instance has its own cache.

```toml
[cache]
backend = "memory"
max_entries = 10000
```

**When to use:** Single-server deployments, development, or when you don't need cross-server cache coherence.

### Redis

Shared cache via Redis. All servers read and write to the same cache, so a write on one server invalidates the cache for all. Requires building with `--features redis`.

```toml
[cache]
backend = "redis"
redis_url = "redis://redis.example.com:6379"
prefix = "crap:"
max_age_secs = 60
```

Keys are automatically prefixed with `prefix` for namespace isolation. When `max_age_secs > 0`, each key is set with a Redis TTL — expired keys are evicted by Redis automatically, and the periodic clear task also runs as a safety net.

**When to use:** Multi-server deployments where stale cache data across servers is unacceptable.

#### Authentication & TLS

Redis credentials and TLS are encoded directly into the connection URL — there is no separate `[cache] password` or `[cache] tls` key. The URL is passed straight through to the `redis` crate:

- **Plain TCP, no auth**: `redis://host:6379`
- **Plain TCP, password**: `redis://:mypassword@host:6379` (note the leading colon for password-only)
- **ACL user (Redis 6+)**: `redis://acl_user:acl_pass@host:6379`
- **TLS**: `rediss://user:pass@host:6380` (double `s` — `rediss`, not `redis`)
- **Specific DB**: append `/<db_number>`, e.g. `redis://host:6379/1`

The same URL is reused by the rate-limit and live-update Redis backends unless they override it.

### None

No-op backend. Cache operations are silently ignored. Each request's relationship population runs fresh queries with no cross-request sharing.

```toml
[cache]
backend = "none"
```

**When to use:** When the database is modified outside the API (direct SQL, external tools) and stale reads are unacceptable, or when debugging cache-related issues.

## Cache Stampede

There is no built-in singleflight / request coalescing on miss. Under concurrent load against a cold (or freshly-invalidated) key, every in-flight request independently hits the database until the first one completes and populates the cache. This is the classic "thundering herd" behavior.

**Impact**: CPU / DB spikes on cold-start, on cache flushes, and immediately after any write (since every write invalidates the cache globally).

**Mitigations**:

- Keep `max_age_secs` long enough that steady-state hit rates are high — short TTLs compound the effect.
- Put a CDN or front-proxy cache in front of public read endpoints so the origin isn't the first line of defense.
- Rate-limit pathological clients at the edge (the load balancer or CDN), not the application, so stampedes can't be induced externally.

This is a **known limitation**. Singleflight / request coalescing is a candidate future enhancement; until then, the design assumes the DB can absorb the worst-case fan-out of one uncached fetch per concurrent reader per invalidation event.

## Cache Invalidation

The cache uses two invalidation strategies:

1. **Write-through invalidation** — every `Create`, `Update`, `Delete`, `Restore`, `UpdateMany`, `DeleteMany`, `UpdateGlobal`, and `RestoreVersion` operation clears the entire cache. This is the primary invalidation mechanism.

2. **Periodic full clear** — when `max_age_secs > 0`, a background task clears the entire cache on a timer. This handles external database mutations that bypass the API.

## Configuration Reference

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `backend` | string | `"memory"` | `"memory"`, `"redis"`, or `"none"` |
| `max_entries` | integer | `10000` | Soft cap for memory backend. New keys are rejected at capacity; existing keys can still be updated. |
| `max_age_secs` | integer | `0` | Periodic clear interval (seconds). `0` = disabled. For Redis, also sets per-key TTL. |
| `redis_url` | string | `"redis://127.0.0.1:6379"` | Redis connection URL. |
| `prefix` | string | `"crap:"` | Key prefix for Redis namespace isolation. |
