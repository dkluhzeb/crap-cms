# Upload

File reference field. Stores a has-one relationship to an upload collection.

## Storage

Upload fields store the referenced media document's ID in a TEXT column on the parent table (same as a has-one relationship).

## Definition

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

The target collection should be an upload collection (defined with `upload = true`).

## API Representation

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

## Admin Rendering

Renders as a `<select>` dropdown listing documents from the target upload collection, using the filename as the display label.
