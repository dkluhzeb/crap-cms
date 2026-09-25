# Image Processing

When an upload collection has `image_sizes` configured, uploaded images are automatically resized and optionally converted to modern formats.

## Image Sizes

Each size definition creates a resized variant of the uploaded image:

```lua
image_sizes = {
    { name = "thumbnail", width = 300, height = 300, fit = "cover" },
    { name = "card", width = 640, height = 480, fit = "contain" },
    { name = "hero", width = 1920, height = 1080, fit = "inside" },
}
```

### Size Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `name` | string | **required** | Size identifier. Used in URLs and field names. |
| `width` | integer | **required** | Target width in pixels |
| `height` | integer | **required** | Target height in pixels |
| `fit` | string | `"cover"` | Resize fit mode |

## Fit Modes

| Mode | Behavior |
|------|----------|
| `cover` | Center-crop the source to the target's aspect ratio, then resize the crop to the target dimensions. No empty space. Aspect ratio preserved. |
| `contain` | Resize to fit within the target dimensions. May be smaller than target. Aspect ratio preserved. |
| `inside` | Same as `contain` — resize to fit within bounds, preserving aspect ratio. |
| `fill` | Stretch to exact target dimensions. Aspect ratio may change. |

`contain` and `inside` scale up as well as down: a source smaller than the box
is enlarged until one side meets it.

Every fit mode plans its passes from the dimensions before touching a pixel,
so the memory a size costs stays within the larger of the source and the
target whatever the source's aspect ratio — a 65,535 × 10 strip resized to a
300 × 300 `cover` size crops a 10 × 10 square first and never builds a
canvas wider than the source.

## Format Options

Generate modern format variants for each image size:

```lua
format_options = {
    webp = { quality = 80 },  -- WebP at 80% quality (80 is also the default when omitted)
    avif = { quality = 60 },  -- AVIF at 60% quality (60 is also the default when omitted)
}
```

| Format | Quality Range | Notes |
|--------|--------------|-------|
| `webp` | 1-100 | Lossy WebP via libwebp |
| `avif` | 1-100 | AVIF via the image crate's AVIF encoder (speed=8) |

Format variants are generated for each image size, not for the original. This keeps original files untouched.

Each encoder has a dimension limit: **16,383 pixels** per side for WebP and
**65,535** for AVIF. A size whose output exceeds it (typically a `contain` /
`inside` size with a very large box, which enlarges) skips that format
variant with a logged warning — the size itself and its other variants are
still stored, and the upload succeeds. With `queue = true` such a variant is
skipped the same way rather than queued as a conversion that could only fail.

### Background Queue

By default, format conversion happens synchronously during upload. For large images or slow formats like AVIF, you can defer conversion to a background queue:

```lua
format_options = {
    webp = { quality = 80 },
    avif = { quality = 60, queue = true },  -- processed in background
}
```

When `queue = true`:

1. The upload completes immediately without generating that format variant
2. A `_system_image_convert` job is enqueued on the `images` queue (stored in the unified `_crap_jobs` table, with the same retry/heartbeat/recovery story as every other job)
3. The scheduler picks up pending jobs and processes them in the background
4. Once complete, the document's URL column is updated with the new file path

Until its job runs, a queued variant's URL column is empty, and a version
snapshot taken with the file records it empty. Every write that makes a stored
file live again queues the variants its columns lack: publishing a draft with
a new file, and restoring a version — so a restored file gets its queued
variants back even though the snapshot never saw them.

This is useful for AVIF which is significantly slower to encode than WebP. The `queue` option is per-format — you can queue AVIF while keeping WebP synchronous. Give image work its own concurrency knob with `[jobs.queues.images]` in `crap.toml`.

Use the [`images` CLI command](../cli/flags.md#images--manage-image-processing-queue) to inspect and manage the queue:

```bash
crap-cms -C ./my-project images stats       # counts by status
crap-cms -C ./my-project images list        # list recent entries
crap-cms -C ./my-project images list -s failed  # show only failed
crap-cms -C ./my-project images retry --all -y  # retry all failed
crap-cms -C ./my-project images purge --older-than 7d  # clean up old entries
```

## Processing Pipeline

For each uploaded image:

1. **Original** — saved as-is to `uploads/<collection>/<nanoid>_<filename>` (a random 10-char nanoid, not the document id, so filenames never collide and are not guessable from the id)
2. **Image dimensions** — read from the decoded image
3. **Per-size variants** — resized according to fit mode, saved in the original format
4. **Format variants** — each sized image is also saved as WebP and/or AVIF (if configured)

Non-image files (PDFs, etc.) skip steps 2-4.

### What derived images keep

Sizes and their WebP/AVIF variants are re-encoded from the decoded pixels, so
they carry only the pixels — the original file keeps everything, byte for
byte:

- **Color profile** — an embedded ICC profile (Display P3, Adobe RGB) is not
  carried over, and the pixels are not converted to sRGB. A wide-gamut photo's
  sizes render slightly desaturated next to the original. Export images in sRGB
  when the sizes must match the original's color exactly. (The profile is not
  copied onto the resized PNG/JPEG either, because the WebP and AVIF variants
  of the same size could not carry it — two renditions of one size would show
  different colors.)
- **Animation** — an animated GIF or WebP produces **static sizes of its first
  frame**. Serve the original when the animation matters.
- **Metadata** — EXIF (GPS coordinates, camera identifiers) is dropped; the
  EXIF orientation is applied to the pixels first, so sizes are upright.

## Decompression Bomb Protection

Before any resizing or format conversion, image dimensions are checked against two
guards. Both protect the server from "decompression bomb" inputs — small, highly
compressed files that decode into enormous bitmaps, which would otherwise consume
gigabytes of RAM during resizing.

1. **Absolute pixel limit — 100 megapixels** (`width × height`). Images exceeding this
   are rejected with `Image too large: 12000x9000 exceeds pixel limit`. For reference: a
   12,000 × 8,000 pixel image (96 MP) is accepted; a 12,000 × 9,000 image (108 MP) is
   rejected.
2. **Compression-ratio cap — 500 pixels per byte.** If the decoded pixel count divided by
   the file's byte length exceeds 500, the image is rejected with
   `Image compression ratio too high: ... Likely a decompression bomb.` This catches files
   that stay under 100 MP but still decode far out of proportion to their size (e.g. a tiny
   header-only stream claiming 8,000 × 8,000). Real photographs sit in the single-digit
   pixels-per-byte range, so normal uploads pass.

Both limits are fixed and not configurable. File-size and MIME-type checks are independent
and run alongside these checks.

## Admin Thumbnail

Set `admin_thumbnail` to the name of an image size to display it in admin list views:

```lua
upload = {
    image_sizes = {
        { name = "thumbnail", width = 300, height = 300, fit = "cover" },
    },
    admin_thumbnail = "thumbnail",
}
```

## Example: Full Media Collection

```lua
crap.collections.define("media", {
    labels = { singular = "Media", plural = "Media" },
    upload = {
        mime_types = { "image/*" },
        max_file_size = 10485760,
        image_sizes = {
            { name = "thumbnail", width = 300, height = 300, fit = "cover" },
            { name = "card", width = 640, height = 480, fit = "cover" },
            { name = "hero", width = 1920, height = 1080, fit = "inside" },
        },
        admin_thumbnail = "thumbnail",
        format_options = {
            webp = { quality = 80 },
            avif = { quality = 60 },
        },
    },
    fields = {
        crap.fields.text({ name = "alt", admin = { description = "Alt text for accessibility" } }),
        crap.fields.textarea({ name = "caption" }),
    },
})
```
