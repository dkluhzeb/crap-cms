# Upload

File reference field. Stores a relationship to an upload collection. Supports both single-file (has-one) and multi-file (has-many) modes.

## Storage

- **Has-one** (default): stores the referenced media document's ID in a TEXT column on the parent table.
- **Has-many** (`has_many = true`): stores references in a junction table `{collection}_{field}`, same as has-many relationships.

## Definition

Single file (has-one):

```lua
crap.fields.upload({
    name = "featured_image",
    relationship = {
        collection = "media",
        max_depth = 1,
    },
})
```

Multi-file (has-many):

```lua
crap.fields.upload({
    name = "gallery",
    relationship = {
        collection = "media",
        has_many = true,
    },
})
```

> **Note:** The flat `relation_to` syntax is deprecated for upload fields too. Use `relationship = { collection = "..." }` instead.

The target collection should be an upload collection (defined with `upload = true`).

## API Representation

### Has-one

At `depth=0`, returns the media document ID as a string:

```json
{
  "featured_image": "abc123"
}
```

At `depth=1+`, the ID is populated with the full media document (same as relationship population):

```json
{
  "featured_image": {
    "id": "abc123",
    "collection": "media",
    "filename": "hero.jpg",
    "mime_type": "image/jpeg",
    "url": "/uploads/media/hero.jpg",
    "sizes": { ... }
  }
}
```

### Has-many

At `depth=0`, returns an array of media document IDs:

```json
{
  "gallery": ["abc123", "def456"]
}
```

At `depth=1+`, each ID is populated with the full media document:

```json
{
  "gallery": [
    { "id": "abc123", "collection": "media", "filename": "hero.jpg", ... },
    { "id": "def456", "collection": "media", "filename": "banner.png", ... }
  ]
}
```

## Admin Rendering

- **Has-one**: renders as a searchable input with filename as the display label and image preview above.
- **Has-many**: renders as a searchable multi-select widget (same as has-many relationships) with chips for selected files.

## Drawer Picker

Upload fields default to `picker = "drawer"`, which shows a browse button next to the search input. Clicking it opens a slide-in drawer panel with a thumbnail grid for visually browsing upload documents.

```lua
-- drawer is the default for upload fields — no need to set it explicitly
crap.fields.upload({
    name = "featured_image",
    relationship = {
        collection = "media",
    },
})
```

- Default (`picker = "drawer"`): inline search + browse button that opens a drawer with thumbnail grid
- `picker = "none"`: inline search autocomplete only (no browse button)
