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
| `mime_type` | text | Hidden | MIME type of the uploaded file |
| `filesize` | number | Hidden | File size in bytes |
| `width` | number | Hidden | Image width (images only) |
| `height` | number | Hidden | Image height (images only) |
| `url` | text | Hidden | URL path to the original file |
| `focal_x` | number | Hidden | Focal point X coordinate (0.0–1.0, default center) |
| `focal_y` | number | Hidden | Focal point Y coordinate (0.0–1.0, default center) |

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

Files are served through the CMS via `/uploads/...` (proxied from S3) so access control and content negotiation work identically to local storage.

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

```lua
crap.storage.register({
  put = function(key, data, content_type)
    crap.http.request({
      method = "PUT",
      url = "https://storage.example.com/" .. key,
      body = data,
      headers = { ["Content-Type"] = content_type },
    })
  end,
  get = function(key)
    local resp = crap.http.request({
      url = "https://storage.example.com/" .. key,
    })
    -- Return nil for a missing key (the CMS serves a 404). Raise an
    -- error only for a real/transient failure (served as a 503), so a
    -- transient outage isn't cached as a permanent "not found".
    if resp.status == 404 then
      return nil
    end
    return resp.body
  end,
  delete = function(key)
    crap.http.request({
      method = "DELETE",
      url = "https://storage.example.com/" .. key,
    })
  end,
  -- Optional: fast existence probe. When omitted, the CMS probes via
  -- `get` (downloading the object), so providing `exists` (e.g. a HEAD
  -- request) is cheaper for large files.
  exists = function(key)
    local resp = crap.http.request({
      method = "HEAD",
      url = "https://storage.example.com/" .. key,
    })
    return resp.status ~= 404
  end,
})
```

```toml
[upload]
storage = "custom"
```

Binary data is passed natively between Rust and Lua (no base64 encoding). The `crap.http.request` function handles binary request/response bodies.

## URL Structure

Files are served at `/uploads/<collection>/<filename>`:

```
/uploads/media/a1b2c3_my-photo.jpg
/uploads/media/a1b2c3_my-photo_thumbnail.webp
```

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

Every upload passes a fixed validation chain before anything is written to storage:

1. **Size** — the file must not exceed the collection's `max_file_size` (or the global `[upload] max_file_size`).
2. **MIME allowlist** — the claimed `Content-Type` must match the collection's `mime_types` patterns.
3. **Magic-byte verification** — the file's leading bytes are sniffed; when the content is recognisable, the detected type must agree with the claimed type (`File content does not match claimed type 'image/png' (detected 'text/html')`), and the detected type is what every later check uses. A renamed `.html` cannot pass as `image/*`.
4. **Extension ↔ content cross-check** — for extensions that resolve to a type a browser would *execute* on serve (HTML, XHTML, SVG, XML, JavaScript) the actual content type must match exactly; a PNG saved as `logo.svg` is rejected. Inert extensions (`.txt`, `.pdf`, `.zip`, …) are not cross-checked because they are served with non-executing content types regardless.
5. **SVG sanitising** — SVG uploads are scanned once for `<!DOCTYPE>` / `<!ENTITY>` declarations and external `xlink:href` loads (XXE and data-exfiltration vectors) and rejected if any are present, so only clean SVGs ever reach storage. Served SVGs additionally carry `Content-Disposition: attachment` and a sandboxing CSP.

For processed images, the EXIF `Orientation` tag is applied before resizing so phone photos come out upright, and the re-encoded outputs (generated sizes and format conversions) carry **no EXIF metadata** — camera details and GPS coordinates are stripped as a side effect of re-encoding. The original upload is stored byte-for-byte.

## Error Cleanup

If an error occurs during upload processing (e.g., image resize fails partway through), all files written so far are automatically cleaned up. This prevents orphaned files from accumulating on disk.

## Content Negotiation

When serving image files, the upload handler performs automatic content negotiation based on the browser's `Accept` header. If a modern format variant exists on disk, it is served instead of the original:

1. **AVIF** — served if the client sends `Accept: image/avif` and a `.avif` variant exists
2. **WebP** — served if the client sends `Accept: image/webp` and a `.webp` variant exists
3. **Original** — served if no matching variant exists

AVIF is preferred over WebP when both are accepted. The response includes a `Vary: Accept` header so caches store format-specific versions correctly.

This works for all image URLs (`/uploads/...`) including originals and resized variants. Non-image files (PDFs, etc.) are always served as-is.

## Focal Point

Upload collections include `focal_x` and `focal_y` fields that store the subject/focus coordinates of an image as floats in the 0.0–1.0 range. Center is `(0.5, 0.5)`.

**Setting in Admin UI:** On the upload collection edit page, click anywhere on the image preview to set the focal point. A crosshair marker shows the current position. The values are saved with the form.

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

When a document in an upload collection is deleted, all associated files (original + resized + format variants) are deleted from disk.
