# Uploading from Client Apps

File uploads in Crap CMS use HTTP multipart form submission. There is no gRPC upload RPC — files must be uploaded via the admin HTTP endpoint.

## Upload Flow

Uploading is a two-step process:

1. **Upload the file** — POST a multipart form to create a document in the upload collection
2. **Reference it** — use the upload document's ID as a relationship field value in other collections

## HTTP Upload Endpoint

```
POST /admin/collections/{slug}
Content-Type: multipart/form-data
Cookie: crap_session=<jwt>
```

Or with Bearer token:

```
POST /admin/collections/{slug}
Content-Type: multipart/form-data
Authorization: Bearer <jwt>
```

### Form Fields

| Field | Type | Description |
|-------|------|-------------|
| `_file` | file | The file to upload (**required**) |
| Any other field | text | Custom fields defined on the collection (e.g., `alt`, `caption`) |

### Example: cURL

```bash
# Upload an image to the "media" collection
curl -X POST http://localhost:3000/admin/collections/media \
  -H "Cookie: crap_session=$(get_session_token)" \
  -F "_file=@/path/to/photo.jpg" \
  -F "alt=A beautiful sunset"
```

### Example: JavaScript (fetch)

```javascript
const form = new FormData();
form.append('_file', fileInput.files[0]);
form.append('alt', 'A beautiful sunset');

const response = await fetch('/admin/collections/media', {
  method: 'POST',
  headers: {
    'Authorization': `Bearer ${token}`,
  },
  body: form,
});
```

### Example: Python (requests)

```python
import requests

files = {'_file': open('photo.jpg', 'rb')}
data = {'alt': 'A beautiful sunset'}

response = requests.post(
    'http://localhost:3000/admin/collections/media',
    files=files,
    data=data,
    cookies={'crap_session': token},
)
```

## Server Processing

When the server receives an upload:

1. **Validates** the MIME type against the collection's `mime_types` allowlist
2. **Checks** file size against `max_file_size`
3. **Sanitizes** the filename (lowercase, hyphens, unique prefix)
4. **Saves** the original file to `uploads/{collection}/{id}_{filename}`
5. **Resizes** images according to `image_sizes` (if configured)
6. **Generates** WebP/AVIF variants (if `format_options` configured)
7. **Creates** a document with all metadata fields populated

## Fetching Upload Documents

After upload, use the gRPC API to fetch the document:

```bash
grpcurl -plaintext -d '{
    "collection": "media",
    "id": "the_upload_id"
}' localhost:50051 crap.ContentAPI/FindByID
```

The response includes structured `sizes` data:

```json
{
    "filename": "a1b2c3_photo.jpg",
    "url": "/uploads/media/a1b2c3_photo.jpg",
    "width": 1920,
    "height": 1080,
    "sizes": {
        "thumbnail": {
            "url": "/uploads/media/a1b2c3_photo_thumbnail.jpg",
            "width": 300,
            "height": 300,
            "formats": {
                "webp": { "url": "/uploads/media/a1b2c3_photo_thumbnail.webp" },
                "avif": { "url": "/uploads/media/a1b2c3_photo_thumbnail.avif" }
            }
        }
    },
    "alt": "A beautiful sunset"
}
```

## Downloading Files

Files are served via HTTP GET:

```
GET /uploads/{collection}/{filename}
```

```bash
# Public file (no access.read configured)
curl http://localhost:3000/uploads/media/a1b2c3_photo_thumbnail.webp

# Protected file (requires auth)
curl http://localhost:3000/uploads/media/a1b2c3_photo.jpg \
  -H "Authorization: Bearer ${token}"
```

### Caching

| Access | Cache-Control |
|--------|--------------|
| Public (no `access.read`) | `public, max-age=31536000, immutable` |
| Protected (`access.read` configured) | `private, no-store` |

## Using Uploads in Other Collections

Reference upload documents via relationship fields:

```lua
-- collections/posts.lua
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text", required = true },
        {
            name = "cover_image",
            type = "relationship",
            relationship = { collection = "media", has_many = false },
        },
    },
})
```

Then when creating a post, pass the upload document's ID:

```bash
# gRPC
grpcurl -plaintext -d '{
    "collection": "posts",
    "data": {
        "title": "My Post",
        "cover_image": "the_upload_id"
    }
}' localhost:50051 crap.ContentAPI/Create
```

With `depth = 1`, the upload document is fully populated in the response, giving you access to all URLs and sizes.

## Authentication

Upload endpoints require the same authentication as any other collection operation. The auth token can be provided via:

- **Cookie**: `crap_session=<jwt>` (set by the admin login flow)
- **Header**: `Authorization: Bearer <jwt>` (from the `Login` gRPC RPC)

Access control on the upload collection's `create` rule applies.
