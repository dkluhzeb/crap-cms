# Upload

File reference field. Stores a relationship to an upload collection. Supports both single-file (has-one) and multi-file (has-many) modes.

## Storage

- **Has-one** (default): stores the referenced media document's ID in a TEXT column on the parent table.
- **Has-many** (`has_many = true`): stores references in a junction table `{collection}_{field}`, same as has-many relationships.

## Definition

Single file (has-one):

```lua
{
    name = "featured_image",
    type = "upload",
    relation_to = "media",
}
```

Or using the expanded relationship syntax:

```lua
{
    name = "featured_image",
    type = "upload",
    relationship = {
        collection = "media",
        max_depth = 1,
    },
}
```

Multi-file (has-many):

```lua
{
    name = "gallery",
    type = "upload",
    relationship = {
        collection = "media",
        has_many = true,
    },
}
```

Or with flat syntax:

```lua
{
    name = "gallery",
    type = "upload",
    relation_to = "media",
    has_many = true,
}
```

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

Add `admin.picker = "drawer"` to enable a browse button next to the search input. Clicking it opens a slide-in drawer panel with a thumbnail grid for visually browsing upload documents.

```lua
{
    name = "featured_image",
    type = "upload",
    relation_to = "media",
    admin = { picker = "drawer" },
}
```

- Without `picker`: inline search autocomplete only (default behavior)
- With `picker = "drawer"`: inline search + browse button that opens a drawer with thumbnail grid
