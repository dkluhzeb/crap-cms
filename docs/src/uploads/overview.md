# Uploads

Upload collections handle file storage with automatic metadata tracking. Enable uploads by setting `upload = true` or providing a config table.

## Configuration

```lua
crap.collections.define("media", {
    labels = { singular = "Media", plural = "Media" },
    upload = {
        mime_types = { "image/*" },
        max_file_size = "10MB",    -- accepts bytes or "10MB", "1GB", etc.
        image_sizes = {
            { name = "thumbnail", width = 300, height = 300, fit = "cover" },
            { name = "card", width = 640, height = 480, fit = "cover" },
        },
        admin_thumbnail = "thumbnail",
        format_options = {
            webp = { quality = 80 },
            avif = { quality = 60 },
        },
    },
    fields = {
        crap.fields.text({ name = "alt", admin = { description = "Alt text" } }),
    },
})
```

## Upload Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `enabled` | boolean | `true` | Set to `false` to disable uploads while keeping the config table (equivalent to `upload = false`). |
| `mime_types` | string[] | `{}` (any) | MIME type allowlist. Supports glob patterns (`"image/*"`). Empty = allow all. |
| `max_file_size` | integer/string | global default | Max file size. Accepts bytes (integer) or human-readable (`"10MB"`, `"1GB"`). Overrides `[upload] max_file_size` in `crap.toml`. |
| `image_sizes` | ImageSize[] | `{}` | Resize definitions for image uploads. See [Image Processing](image-processing.md). |
| `admin_thumbnail` | string | `nil` | Name of an `image_sizes` entry to use as thumbnail in admin lists. |
| `format_options` | table | `{}` | Auto-generate format variants. See [Image Processing](image-processing.md). |

## Auto-Injected Fields

When uploads are enabled, these fields are automatically injected before your custom fields:

| Field | Type | Admin form | Description |
|-------|------|------------|-------------|
| `filename` | text | Visible (readonly) | Sanitized filename with unique prefix |
| `mime_type` | text | Hidden | MIME type of the uploaded file — the type sniffed from its bytes when they are recognisable, the claimed type otherwise |
| `filesize` | number | Hidden | File size in bytes |
| `width` | number | Hidden | Image width (images only) |
| `height` | number | Hidden | Image height (images only) |
| `url` | text | Hidden | URL path to the original file |
| `focal_x` | number | Hidden | Focal point X coordinate (0.0–1.0, default center; a value outside the range is a validation error on every surface) |
| `focal_y` | number | Hidden | Focal point Y coordinate (0.0–1.0, default center; a value outside the range is a validation error on every surface) |

> All auto-injected fields are **always returned in API responses** (gRPC, Lua, MCP, REST, admin JSON). They use `admin.hidden = true` only — that flag suppresses standard form rendering because the upload preview widget and focal-point selector render these values directly. Consumers (and the admin's own preview widget) need them to display thumbnails, focal crops, and image variants.

For each image size, additional fields are injected:

| Field Pattern | Type | Description |
|--------------|------|-------------|
| `{size}_url` | text | URL to the resized image |
| `{size}_width` | number | Actual width after resize |
| `{size}_height` | number | Actual height after resize |
| `{size}_webp_url` | text | URL to WebP variant (if enabled) |
| `{size}_avif_url` | text | URL to AVIF variant (if enabled) |

## File Storage

By default, files are stored on the local filesystem at `<config_dir>/uploads/<collection_slug>/`:

```
uploads/
└── media/
    ├── a1b2c3_my-photo.jpg          # original
    ├── a1b2c3_my-photo_thumbnail.jpg # resized
    ├── a1b2c3_my-photo_thumbnail.webp
    ├── a1b2c3_my-photo_thumbnail.avif
    ├── a1b2c3_my-photo_card.jpg
    ├── a1b2c3_my-photo_card.webp
    └── a1b2c3_my-photo_card.avif
```

Filenames are sanitized (lowercase, characters that are not alphanumeric, hyphens, or underscores are replaced with hyphens) and prefixed with a random 10-character nanoid.

### Storage Backends

The storage backend is configurable via `[upload] storage` in `crap.toml`. Local filesystem is the default and recommended for most deployments.

#### Local (default)

```toml
[upload]
storage = "local"  # or omit — local is the default
```

No additional configuration needed. Files stored at `{config_dir}/uploads/`.

#### S3-Compatible (optional)

For multi-server deployments where multiple instances need to share uploaded files. Works with AWS S3, MinIO, Cloudflare R2, Backblaze B2, and DigitalOcean Spaces. Requires `--features s3-storage` at build time.

```toml
[upload]
storage = "s3"

[upload.s3]
bucket = "my-uploads"
region = "us-east-1"
endpoint = "https://s3.amazonaws.com"    # or MinIO/R2 URL
access_key = "${AWS_ACCESS_KEY}"
secret_key = "${AWS_SECRET_KEY}"
prefix = ""                              # optional key prefix
path_style = false                       # true for MinIO
```

| Field | Required | Description |
|-------|----------|-------------|
| `bucket` | Yes | S3 bucket name |
| `region` | No | AWS region (default: `us-east-1`) |
| `endpoint` | No | Custom endpoint for non-AWS providers |
| `access_key` | Yes | AWS access key ID |
| `secret_key` | Yes | AWS secret access key |
| `prefix` | No | Key prefix prepended to all storage keys |
| `path_style` | No | Use path-style URLs (required for MinIO) |

Files are served through the CMS via `/uploads/...` (proxied from S3) so access control and content negotiation work identically to local storage: range requests (`206` with `Content-Range`, `416` for an unsatisfiable range, `If-Range` honoured), strong ETags, conditional `304` responses and `412` preconditions (see [Conditional requests](#conditional-requests)) behave the same on every backend.

Against S3 every request starts with a `HEAD`: a failed precondition (`If-Match` / `If-Unmodified-Since`) is answered `412` and a conditional hit (`If-None-Match` / `If-Modified-Since`) `304` from it without transferring the object, and an unsatisfiable range `416`. The body — the whole object or the requested range, open-ended (`bytes=100-`) and suffix (`bytes=-500`) ranges included — is then streamed as ranged reads of at most 8 MiB, each fetched only when the connection asks for the next one, so a download holds at most a chunk or two in memory whatever the file size, a `HEAD` request reads no body at all, and a client that disconnects stops the reads. If the object is replaced while it streams (its ETag changes), the transfer is aborted rather than splicing two versions. A `Range` whose `If-Range` names an older version is ignored and the whole current file is served.

A custom Lua backend is served the same way when it registers the optional `stat` and `get_range` handlers (see below). Without them its `get` returns whole objects, so each request holds the whole object in memory once (as the Lua string the handler returned), a conditional request reads it before answering `304` (or `412`), and ranges are sliced in the CMS — keep such a backend's files small.

An uploaded filename is capped at 200 characters after sanitising (the stored key adds the id prefix and any size suffix), and two `image_sizes` entries may not share a name — they would resolve to the same stored key.

> **Tip:** Use `queue: true` on image format options (WebP, AVIF) when using S3. Deferred processing avoids upload latency from the extra S3 round trips.

#### Custom (Lua)

For exotic storage providers, register custom functions in `init.lua`:

> **The handlers must be stateless** — they have to delegate to an external
> store (over `crap.http`), not keep files in Lua memory. The hook runner
> uses a *pool* of independent Lua VMs (pre-warming `[hooks] vm_pool_size`,
> growing up to `max_vm_pool_size`), each running `init.lua` separately, and
> a different VM may serve each request.
> A handler that stashed bytes in a Lua table would `put` into one VM and
> `get` a miss from another. Use `crap.env` for credentials and
> `crap.crypto` (HMAC/SHA-256) if the provider needs request signing.

> **Handlers that write files with Lua `io`** (for example a custom backend
> that stores objects on a mounted volume) can only reach the config
> directory and the directories listed in
> [`[hooks] io_roots`](../configuration/crap-toml.md#hooks) — see
> [the hook sandbox](../hooks/overview.md). A storage directory outside the
> config directory must be listed there:
>
> ```toml
> [hooks]
> io_roots = ["/srv/crap-media"]
> ```
>
> Even inside an allowed root, `data/` (database, generated auth secret),
> `backups/`, `crap.toml`, the log directory and the database files are
> refused, so a custom backend cannot store objects under the data
> directory. Use a directory of its own.

```lua
local base = "https://storage.example.com/"

--- Raise on a status the provider uses for failure, so the write fails
--- (or the read is served as a retryable 503) instead of passing silently.
local function check(resp, what, key)
  if resp.status >= 300 then
    error(what .. " " .. key .. " failed with HTTP " .. resp.status)
  end
  return resp
end

crap.storage.register({
  put = function(key, data, content_type)
    check(crap.http.request({
      method = "PUT",
      url = base .. key,
      body = data,
      headers = { ["Content-Type"] = content_type },
    }), "put", key)
  end,
  get = function(key)
    local resp = crap.http.request({ url = base .. key })
    -- Return nil for a missing key (the CMS serves a 404). Raise an
    -- error only for a real/transient failure (served as a 503), so a
    -- transient outage isn't cached as a permanent "not found".
    if resp.status == 404 then
      return nil
    end
    return check(resp, "get", key).body
  end,
  delete = function(key)
    local resp = crap.http.request({ method = "DELETE", url = base .. key })
    if resp.status ~= 404 then
      check(resp, "delete", key)
    end
  end,
  -- Optional: fast existence probe. When omitted, the CMS asks `stat`
  -- (when registered) or else probes via `get` (downloading the object),
  -- so providing `exists` (e.g. a HEAD request) is cheaper for large files.
  exists = function(key)
    local resp = crap.http.request({ method = "HEAD", url = base .. key })
    if resp.status == 404 then
      return false
    end
    check(resp, "exists", key)
    return true
  end,
  -- Optional, together with `get_range`: the object's metadata without its
  -- bytes (nil when missing). With both, downloads are streamed.
  stat = function(key)
    local resp = crap.http.request({ method = "HEAD", url = base .. key })
    if resp.status == 404 then
      return nil
    end
    check(resp, "stat", key)
    return {
      size = tonumber(resp.headers["content-length"]),
      etag = resp.headers["etag"],
      last_modified = resp.headers["last-modified"],
    }
  end,
  -- Exactly the bytes `first`..`last` (0-based, inclusive; nil when missing).
  get_range = function(key, first, last)
    local resp = crap.http.request({
      url = base .. key,
      headers = { Range = "bytes=" .. first .. "-" .. last },
    })
    if resp.status == 404 then
      return nil
    end
    if resp.status ~= 206 then
      error("get_range " .. key .. ": expected HTTP 206, got " .. resp.status)
    end
    return resp.body
  end,
})
```

`stat` returns a table with `size` (bytes, required), and optionally `etag` (an opaque version string that changes whenever the bytes change; surrounding quotes are stripped) and `last_modified` (Unix seconds or an HTTP-date string). Unknown keys, a negative size, an `etag` with quotes or control characters inside, or an unparseable date are errors. `get_range` must return exactly `last - first + 1` bytes — a shorter or longer string aborts the response rather than serving a corrupted file. The two are accepted only together; registering one without the other is an error at startup. With them, the serve route answers preconditions (`412`), conditional requests (`304`) and unsatisfiable ranges (`416`) from `stat` alone and streams the body in ranged reads of at most 8 MiB, calling `stat` again with every read so an object replaced mid-download (its `etag` changes) aborts the transfer. Without an `etag`, the CMS derives one from the key, size and `last_modified`.

```toml
[upload]
storage = "custom"
```

Binary data is passed natively between Rust and Lua (no base64 encoding). The `crap.http.request` function handles binary request/response bodies.

### Storage guarantees

- **Local:** a file is staged beside its final path, synced and renamed into
  place, so a key never holds a partial object after a crash. A stray
  `.<name>.<pid>.<n>.crap-tmp` sibling is a leftover from a killed process and
  safe to delete.
- **S3:** every request's HTTP status is checked. A rejected upload (403, 5xx)
  fails the write instead of committing a document that points at an object
  that was never stored; a rejected read fails instead of serving the
  provider's error body as the file; a rejected delete fails instead of
  silently leaving the object; `exists` reports a missing object as absent.
  Errors name the operation, key and status, never the response body.
- **Custom:** `[upload] storage = "custom"` needs the Lua runtime; creating the
  storage without one is an error, never a silent fallback to local storage.
- **Replacing a file** cancels the previous file's queued image conversions;
  a conversion already running when its file is replaced discards its output.
  Every server-derived column the new file does not produce is cleared —
  a PDF replacing a JPEG nulls `width`, `height` and every per-size URL
  instead of leaving the old image's values behind.
  A stored file is deleted only when nothing references it any more — not the
  live row, not a draft, not a version snapshot — after the write commits;
  pruning versions releases the files they were the last reference to, and a
  hard delete removes every file the document's row or snapshots ever named.
  A draft save with a new file leaves the published file in place; the
  drafted file becomes live when the draft is published. Until then it is
  served to exactly the viewers whose draft view shows that draft — the
  editor's preview works, a reader without draft access gets `404`.
- **Restoring a version** makes the snapshot's file live again, with the
  format variants that snapshot recorded empty: a variant converted on the
  background queue (`queue = true`) is filled in on the live row by its job,
  never in the snapshot taken when the file was stored. A variant whose bytes
  the document still holds (the live row or another version names it) is
  named again as it is; any other is queued for conversion again. A restore
  that swaps the file cancels the previous file's still-queued conversions,
  like any other replacement.

## URL Structure

Files are served at `/uploads/<collection>/<filename>`:

```
/uploads/media/a1b2c3_my-photo.jpg
/uploads/media/a1b2c3_my-photo_thumbnail.webp
```

### Conditional requests

The serve route evaluates the request's conditions in the order RFC 9110
sets, on every storage backend:

1. **`If-Match`** — the file's entity tag must match one of the listed tags
   under the *strong* comparison (a weak `W/"…"` tag never matches; `*`
   matches any existing file). Otherwise: `412 Precondition Failed`.
2. **`If-Unmodified-Since`** — only when no `If-Match` was sent: a file
   modified after the date answers `412`. An invalid date, or a file without
   a modification date, ignores the header.
3. **`If-None-Match`** / **`If-Modified-Since`** — a match answers `304 Not
   Modified` (the entity tag decides alone when both are sent).
4. **`Range`** / **`If-Range`** — only then is a range honoured (`206`), and
   only while `If-Range` still names the current file.

A client that pins a file — a resumed download, a sync tool — therefore
never receives a different version: it gets `412` and can start over.
Files on local storage carry no entity tag (only `Last-Modified`), so there
an `If-Match` listing tags always answers `412`; use `If-Unmodified-Since`
(or `If-Range`) with local storage. On S3 and on a custom backend with a
`stat` handler the preconditions are judged from the object's metadata,
before any byte is read.

Access-gated files can additionally be served via short-lived **signed
URLs** (`?exp=…&sig=…`, minted with `crap.uploads.sign_url`) for
cross-origin or CDN delivery — see
[Downloading Files](client-uploads.md#signed-urls).

## API Response

The `sizes` field in API responses is a structured object assembled from the per-size columns:

```json
{
    "url": "/uploads/media/a1b2c3_my-photo.jpg",
    "filename": "a1b2c3_my-photo.jpg",
    "sizes": {
        "thumbnail": {
            "url": "/uploads/media/a1b2c3_my-photo_thumbnail.jpg",
            "width": 300,
            "height": 300,
            "formats": {
                "webp": { "url": "/uploads/media/a1b2c3_my-photo_thumbnail.webp" },
                "avif": { "url": "/uploads/media/a1b2c3_my-photo_thumbnail.avif" }
            }
        }
    }
}
```

## MIME Type Patterns

| Pattern | Matches |
|---------|---------|
| `"image/*"` | All image types (png, jpeg, gif, webp, etc.) |
| `"application/pdf"` | Only PDF files |
| `"*/*"` or `"*"` | Any file type |

Empty `mime_types` array also accepts any file.

## Upload Validation

Every upload passes a fixed validation chain before anything is written to storage. Once the chain has passed — and still before a byte is stored or an image resized — the collection's `create` (or, for a replacement, `update`) access rule is consulted on the request's fields together with the columns the file already determines: `filename` (the stored name), `mime_type` (the detected type), `filesize`, and `width` / `height` for an image the server decodes. A caller the rule refuses is refused as access denied without the file being stored, so a rule may gate uploads on `ctx.data.mime_type` or `ctx.data.filesize`. The columns that only exist once the file is stored — `url` and every per-size column — are empty at that point; the write consults the rule again on the final data, where they are set.

1. **Size** — the file must not exceed the collection's `max_file_size` (or the global `[upload] max_file_size`). On the routes that carry a file — the collection's admin create/update and the `/api/upload` routes — the request body limit follows that collection's limit (plus 1 MiB for the other form fields), so a collection may allow larger files than the global default. Every other route keeps the global limit plus 1 MiB. In the admin UI the file input refuses an oversized pick as soon as it is made; a request that exceeds the body limit anyway is answered `413` with an error toast, and the form keeps every edit.
2. **MIME allowlist** — the claimed `Content-Type` must be one concrete `type/subtype` (a pattern such as `image/*` or `*/*` is rejected) and must match the collection's `mime_types` patterns.
3. **Magic-byte verification** — the file's leading bytes are sniffed; when the content is recognisable, the detected type must agree with the claimed type (`File content does not match claimed type 'image/png' (detected 'text/html')`), and the detected type is what every later check uses and what `mime_type` stores. A renamed `.html` cannot pass as `image/*`.
4. **Extension ↔ content cross-check** — for extensions that resolve to a type a browser would *execute* on serve (HTML, XHTML, SVG, XML, JavaScript) the actual content type must match exactly; a PNG saved as `logo.svg` is rejected. The extension is read from the *sanitized* name — the one stored and served — so `evil.htm l`, stored as `evil.html`, is judged as HTML. Inert extensions (`.txt`, `.pdf`, `.zip`, …) are not cross-checked because they are served with non-executing content types regardless.
5. **SVG sanitising** — SVG uploads are scanned once for `<script>` elements, inline event handlers (`onload=` …), `<!DOCTYPE>` / `<!ENTITY>` declarations, CSS `@import`, and external references in `href` / `xlink:href` or `url(…)` (any scheme other than `data:` — `mailto:` and `tel:` included — or a protocol-relative `//` URL, judged after entity decoding). Any hit rejects the upload (stored-XSS, XXE and data-exfiltration vectors), so only clean SVGs ever reach storage; fragments, relative paths and `data:` URIs are fine. Served SVGs additionally carry `Content-Disposition: attachment` and a sandboxing CSP (`sandbox; default-src 'none'`) — which the admin's own Content-Security-Policy never replaces.

For processed images, the EXIF `Orientation` tag is applied before resizing so phone photos come out upright, and the re-encoded outputs (generated sizes and format conversions) carry **no EXIF metadata** — camera details and GPS coordinates are stripped as a side effect of re-encoding. The original upload is stored byte-for-byte. They also carry no ICC color profile, and an animated GIF or WebP yields static first-frame sizes — see [What derived images keep](image-processing.md#what-derived-images-keep).

Only formats the server can decode — JPEG, PNG, GIF and WebP — go through the pixel pipeline (dimensions, generated sizes, format conversions). SVG, AVIF and any other allow-listed image type are stored verbatim: no dimensions are recorded and no variants are generated.

## Error Cleanup

If an error occurs during upload processing (e.g., image resize fails partway through), all files written so far are automatically cleaned up. This prevents orphaned files from accumulating on disk.

## Content Negotiation

When serving image files, the upload handler performs automatic content negotiation based on the browser's `Accept` header. If a modern format variant of the requested file exists, it is served instead:

1. **AVIF** — served if the client sends `Accept: image/avif` and a `.avif` variant exists
2. **WebP** — served if the client sends `Accept: image/webp` and a `.webp` variant exists
3. **Original** — served if no matching variant exists

AVIF is preferred over WebP when both are accepted. The response includes a `Vary: Accept` header so caches store format-specific versions correctly.

Negotiation applies to the generated size files only (`…_thumbnail.jpg`), and only for the formats the collection's `format_options` configure — variants are never generated for the original, so an original, a non-image file, or a collection without `format_options` is served as-is with no variant lookup and no `Vary: Accept`.

## Focal Point

Upload collections include `focal_x` and `focal_y` fields that store the subject/focus coordinates of an image as floats in the 0.0–1.0 range. Center is `(0.5, 0.5)`.

**Setting in Admin UI:** On the upload collection edit page, click anywhere on the image preview to set the focal point — or focus the preview (Tab) and move it with the arrow keys (hold Shift for a finer step). A crosshair marker shows the current position. The values are saved with the form, and moving the point counts as an unsaved change.

**Frontend usage:** Use the coordinates with CSS `object-position` to keep the subject in frame when cropping at different aspect ratios:

```css
.responsive-image {
  object-fit: cover;
  object-position: calc(var(--focal-x) * 100%) calc(var(--focal-y) * 100%);
}
```

Or inline from template data:

```html
<img src="/uploads/media/photo.jpg"
     style="object-fit: cover; object-position: 50% 30%;" />
```

The values are available in API responses as `focal_x` and `focal_y` number fields.

## File Deletion

- **Soft delete** (collections with `soft_delete = true`) moves the document to the trash and keeps every file: a trashed document's files stop serving (the serve gate excludes trashed rows) and come back with a restore from the trash.
- **Hard delete** — a delete on a collection without soft delete, emptying the trash, or the retention purge — removes every file the document owns once the delete commits: the original, its resized sizes and format variants, and every file a draft or version snapshot of the document named.
