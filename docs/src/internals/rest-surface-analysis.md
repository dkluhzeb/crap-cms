# REST Surface — Analysis (updated)

> **Status: analysis only — the standing product decision is unchanged.**
> REST stays unbuilt until real user pressure exists, and then ships behind a
> non-default `rest-api` feature flag. gRPC remains the primary API. This
> page replaces the earlier informal analysis, which had two gaps: it
> predated the single-source wire model, and it never considered **live
> behavior** — what a REST client does when gRPC clients would use
> `Subscribe`.

## What changed since the first analysis

Three structural changes collapsed most of REST's original cost:

1. **The operation core.** Every surface is now a thin codec over
   `service::op` (`auth → ctx → op → encode`). A REST surface is one more
   codec — it cannot fork business logic, access semantics, or validation,
   because those live below the codec line.
2. **The single-source wire model** (`service::op::wire`). The original
   analysis scored REST as "a fourth hand-maintained wire description that
   will drift." That cost is now near zero: REST's parameter surface would
   be *declared in the same model* that renders the MCP schemas and
   generates the proto messages. A REST route/param table and an **OpenAPI
   document** become one more emitter (`cargo xtask gen-openapi --check`),
   drift-gated in CI like the proto.
3. **The MCP JSON-schema emitter.** Def-dependent field schemas
   (`fields_to_object_schema`) already exist in JSON-Schema form — exactly
   what OpenAPI components need. Generating a *per-project* OpenAPI spec
   (from the user's actual collections) is mostly assembly, not new code.

## The live-behavior gap (the axis the first analysis missed)

A REST API without a live story is not at parity with the gRPC surface —
`Subscribe` is a first-class feature (live updates, cache invalidation,
admin live-refresh all ride `MutationEvent`s). Any REST plan must answer it.

### What already exists

| Surface | Transport | Scope |
|---|---|---|
| gRPC `Subscribe` | HTTP/2 server-streaming, JWT in request | public API |
| Admin `/admin/events` | SSE, cookie auth, slot-limited (`live.max_sse_connections`), per-subscriber access filtering (`SseAccess`, `build_allowed_slugs`) | admin UI |
| Event fan-out | in-process broadcast + Redis transport (multi-server) | shared by both |

### SSE, not WebSocket

The instinct "REST needs WebSocket" is wrong for this product, for a
structural reason: **the subscription model is unidirectional.** A
`SubscribeRequest` declares its filters once, up front; there is no
client→server traffic after that — gRPC `Subscribe` is *server*-streaming,
not bidirectional. WebSocket's one capability over SSE (client→server
messages on the same connection) buys nothing today, while costing:

- a second auth path (WS upgrade headers vs. plain HTTP),
- proxy/load-balancer fragility SSE doesn't have (SSE is plain HTTP),
- hand-rolled keepalive/reconnect (SSE has `retry:` and `Last-Event-ID`
  built into `EventSource`),
- a protocol frame layer with no existing infrastructure here.

A public **`/api/events` SSE endpoint** is the correct pairing: it reuses
the admin SSE machinery (pump task, slot limiter, cancellable stream) with
`SubscribeAccess`-style filtering and Bearer/cookie auth. Browsers consume
it natively via `EventSource` — no SDK required, which is the whole point
of a REST surface.

Revisit WebSocket only if a genuinely bidirectional feature appears
(e.g. mutating subscription filters mid-stream, client presence). None is
planned; [event-delivery future work](../live-updates/overview.md) —
coalescing, replay, queue backends — is transport-agnostic and would land
under SSE the same as under gRPC.

One real gap either way: **replay**. Neither `Subscribe` nor admin SSE
supports resume-after-disconnect; SSE's `Last-Event-ID` gives REST a
natural slot for it if event replay is ever built. Absence of replay is
today's shared behavior, not a REST regression.

### The browser alternative that needs no REST at all

`tonic-web` (gRPC-Web) supports unary **and server-streaming** calls over
HTTP/1.1 — a browser can consume `Find` *and* `Subscribe` through the
existing gRPC service with one added tower layer and zero new contract.
This remains the cheapest browser story and the planned basis for the TS
SDK. Its limits are exactly REST's selling points in reverse: opaque binary
payloads, no `curl`/no REPL-ability, SDK required, unfamiliar to the
webhook/integration ecosystem that asks for "a REST API."

## Proposed shape (when the demand trigger fires)

- **Feature flag**: `rest-api`, off by default; router mounted beside gRPC.
- **Routes from the wire model** — verbs/paths declared per op next to the
  pinned proto tags, e.g.:

  | Op | Route |
  |---|---|
  | find / count | `GET /api/{collection}` / `…/count` |
  | find_by_id | `GET /api/{collection}/{id}` |
  | create / update / delete | `POST` / `PATCH` / `DELETE /api/{collection}[/{id}]` |
  | undelete / unpublish / versions | `POST …/{id}/undelete` etc. |
  | globals | `GET/PATCH /api/globals/{slug}` |
  | validate | `POST …/validate` |
  | live | `GET /api/events` (SSE) |

- **Params**: query params for read options, JSON body for writes — names
  are the wire model's names, checked by the same parity tests.
- **Auth**: `Authorization: Bearer` (same JWT as gRPC) + cookie fallback.
- **Errors**: one mapping `CoreError → HTTP status + application/problem+json`,
  defined once beside `core_error_status` (the gRPC mapping), so the two
  stay symmetrical.
- **OpenAPI**: `cargo xtask gen-openapi [--check]` emitting the op-level
  spec from the wire model; a runtime endpoint can serve the def-dependent
  variant (reusing the MCP field-schema emitter) for the user's project.
- **Estimated cost**: the codec layer is mechanical (one handler per op,
  each ~the size of a gRPC handler); SSE endpoint is a lift-and-generalize
  of `admin/handlers/events`; OpenAPI emitter is a fourth wire-model
  consumer. The enduring cost is surface support (docs, issues, versioning
  discipline) — which is why the demand gate stays.

## Decision matrix

| Option | Cost now | Browser story | Ecosystem story | Live story |
|---|---|---|---|---|
| Status quo (gRPC only) | 0 | SDK later via tonic-web | weak for integrations | `Subscribe` |
| + tonic-web layer | tiny | good (SDK-mediated) | unchanged | `Subscribe` via gRPC-Web |
| + REST (`rest-api` flag) | moderate, mostly generated | native (`fetch`/`EventSource`) | strong (curl, webhooks, tools) | SSE `/api/events` |

**Recommendation**: unchanged — hold REST behind the demand gate; ship the
tonic-web layer whenever the TS SDK work starts (it is nearly free and
independently useful). When the gate opens, build REST per this page:
wire-model-generated routes + OpenAPI, problem+json errors, and SSE for
live — **no WebSocket**.
