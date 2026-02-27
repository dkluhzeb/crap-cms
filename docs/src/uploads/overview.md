# Uploads

Upload collections handle file storage with automatic metadata tracking. Enable uploads by setting `upload = true` or providing a config table.

## Configuration

```lua
crap.collections.define("media", {
    labels = { singular = "Media", plural = "Media" },
    upload = {
        mime_types = { "image/*" },
        max_file_size = 10485760,  -- 10 MB
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
        { name = "alt", type = "text", admin = { description = "Alt text" } },
    },
})
```

## Upload Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `mime_types` | string[] | `{}` (any) | MIME type allowlist. Supports glob patterns (`"image/*"`). Empty = allow all. |
| `max_file_size` | integer | global default | Max file size in bytes. Overrides `[upload] max_file_size` in `crap.toml`. |
| `image_sizes` | ImageSize[] | `{}` | Resize definitions for image uploads. See [Image Processing](image-processing.md). |
| `admin_thumbnail` | string | `nil` | Name of an `image_sizes` entry to use as thumbnail in admin lists. |
| `format_options` | table | `{}` | Auto-generate format variants. See [Image Processing](image-processing.md). |

## Auto-Injected Fields

When uploads are enabled, these fields are automatically injected before your custom fields:

| Field | Type | Hidden | Description |
|-------|------|--------|-------------|
| `filename` | text | No (readonly) | Sanitized filename with unique prefix |
| `mime_type` | text | Yes | MIME type of the uploaded file |
| `filesize` | number | Yes | File size in bytes |
| `width` | number | Yes | Image width (images only) |
| `height` | number | Yes | Image height (images only) |
| `url` | text | Yes | URL path to the original file |

For each image size, additional fields are injected:

| Field Pattern | Type | Description |
|--------------|------|-------------|
| `{size}_url` | text | URL to the resized image |
| `{size}_width` | number | Actual width after resize |
| `{size}_height` | number | Actual height after resize |
| `{size}_webp_url` | text | URL to WebP variant (if enabled) |
| `{size}_avif_url` | text | URL to AVIF variant (if enabled) |

## File Storage

Files are stored at `<config_dir>/uploads/<collection_slug>/`:

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

Filenames are sanitized (lowercase, non-alphanumeric characters replaced with hyphens) and prefixed with a random 10-character nanoid.

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

## Error Cleanup

If an error occurs during upload processing (e.g., image resize fails partway through), all files written so far are automatically cleaned up. This prevents orphaned files from accumulating on disk.

## File Deletion

When a document in an upload collection is deleted, all associated files (original + resized + format variants) are deleted from disk.
