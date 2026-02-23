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
| `cover` | Resize to fill the target dimensions, then center-crop. No empty space. Aspect ratio preserved. |
| `contain` | Resize to fit within the target dimensions. May be smaller than target. Aspect ratio preserved. |
| `inside` | Same as `contain` — resize to fit within bounds, preserving aspect ratio. |
| `fill` | Stretch to exact target dimensions. Aspect ratio may change. |

## Format Options

Generate modern format variants for each image size:

```lua
format_options = {
    webp = { quality = 80 },  -- WebP at 80% quality
    avif = { quality = 60 },  -- AVIF at 60% quality
}
```

| Format | Quality Range | Notes |
|--------|--------------|-------|
| `webp` | 1-100 | Lossy WebP via libwebp |
| `avif` | 1-100 | AVIF via the image crate's AVIF encoder (speed=8) |

Format variants are generated for each image size, not for the original. This keeps original files untouched.

## Processing Pipeline

For each uploaded image:

1. **Original** — saved as-is to `uploads/<collection>/<id>_<filename>`
2. **Image dimensions** — read from the decoded image
3. **Per-size variants** — resized according to fit mode, saved in the original format
4. **Format variants** — each sized image is also saved as WebP and/or AVIF (if configured)

Non-image files (PDFs, etc.) skip steps 2-4.

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
        { name = "alt", type = "text", admin = { description = "Alt text for accessibility" } },
        { name = "caption", type = "textarea" },
    },
})
```
