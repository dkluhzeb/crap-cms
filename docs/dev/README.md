# Development Documentation

Internal engineering documents: migration plans, architecture proposals,
product-decision analyses, and cross-surface consistency tracking. They are
**not part of the rendered user documentation** (the mdbook under
`docs/src/`) — they describe how and why the system was built, not how to
use it.

| Document | Kind | Status |
|----------|------|--------|
| [Operation Core — Migration Plan](operation-core-migration.md) | migration plan | all stages landed (Aug 2026) |
| [Performance Architecture — Plan](performance-architecture.md) | proposal | partially landed; rest deferred |
| [Lua Connection Injection — Hardening Plan](lua-connection-injection.md) | proposal | deferred (no known bug) |
| [REST Surface — Analysis](rest-surface-analysis.md) | product analysis | decision: REST stays unbuilt |
| [API Surface Comparison](api-surface-comparison.md) | consistency tracking | maintained as surfaces change |

User-facing "how it works" pages (database, cache, frozen contracts) stay in
the book's *Internals* section.
