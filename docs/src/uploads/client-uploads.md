# Uploading from Client Apps

File uploads in Crap CMS use dedicated HTTP endpoints that accept multipart form data and return JSON. These are separate from the admin UI routes and designed for programmatic use.

## Upload API Endpoints

| Method | Route | Description |
|--------|-------|-------------|
| `POST` | `/api/upload/{slug}` | Upload file + create document |
| `PATCH` | `/api/upload/{slug}/{id}` | Replace file on existing document |
| `DELETE` | `/api/upload/{slug}/{id}` | Delete document + files |

All endpoints require authentication via `Authorization: Bearer <jwt>` header and return JSON responses.

## Upload Flow

Uploading is a two-step process:

1. **Upload the file** — POST a multipart form to create a document in the upload collection
2. **Reference it** — use the upload document's ID as a relationship field value in other collections

## Creating an Upload

```
POST /api/upload/{slug}
Content-Type: multipart/form-data
Authorization: Bearer <jwt>
```

### Form Fields

| Field | Type | Description |
|-------|------|-------------|
| `_file` | file | The file to upload (**required**) |
| Any other field | text | Custom fields defined on the collection (e.g., `alt`, `caption`) |

### Response

```
201 Created
Content-Type: application/json
```

```json
{
    "document": {
        "id": "abc123",
        "filename": "a1b2c3_photo.jpg",
        "mime_type": "image/jpeg",
        "filesize": 245760,
        "url": "/uploads/media/a1b2c3_photo.jpg",
        "width": 1920,
        "height": 1080,
        "alt": "A beautiful sunset",
        "created_at": "2025-01-15T10:30:00Z",
        "updated_at": "2025-01-15T10:30:00Z"
    }
}
```

### Example: cURL

```bash
curl -X POST http://localhost:3000/api/upload/media \
  -H "Authorization: Bearer $TOKEN" \
  -F "_file=@/path/to/photo.jpg" \
  -F "alt=A beautiful sunset"
```

### Example: JavaScript (fetch)

```javascript
const form = new FormData();
form.append('_file', fileInput.files[0]);
form.append('alt', 'A beautiful sunset');

const response = await fetch('/api/upload/media', {
  method: 'POST',
  headers: {
    'Authorization': `Bearer ${token}`,
  },
  body: form,
});
const { document } = await response.json();
console.log(document.url); // /uploads/media/a1b2c3_photo.jpg
```

### Example: Python (requests)

```python
import requests

files = {'_file': open('photo.jpg', 'rb')}
data = {'alt': 'A beautiful sunset'}

response = requests.post(
    'http://localhost:3000/api/upload/media',
    files=files,
    data=data,
    headers={'Authorization': f'Bearer {token}'},
)
doc = response.json()['document']
```

## Replacing a File

Replace the file on an existing upload document. Old files are cleaned up on success.

```
PATCH /api/upload/{slug}/{id}
Content-Type: multipart/form-data
Authorization: Bearer <jwt>
```

The form fields are the same as create. The `_file` field is optional — if omitted, only the metadata fields are updated.

```bash
curl -X PATCH http://localhost:3000/api/upload/media/abc123 \
  -H "Authorization: Bearer $TOKEN" \
  -F "_file=@/path/to/new-photo.jpg" \
  -F "alt=Updated caption"
```

### Response

```json
{
    "document": {
        "id": "abc123",
        "filename": "x9y8z7_new-photo.jpg",
        "url": "/uploads/media/x9y8z7_new-photo.jpg",
        "alt": "Updated caption",
        "updated_at": "2025-01-15T11:00:00Z"
    }
}
```

## Deleting an Upload

Delete an upload document and all associated files (original + resized + format variants).

```
DELETE /api/upload/{slug}/{id}
Authorization: Bearer <jwt>
```

```bash
curl -X DELETE http://localhost:3000/api/upload/media/abc123 \
  -H "Authorization: Bearer $TOKEN"
```

### Response

```json
{
    "success": true
}
```

## Error Responses

All error responses follow the same format:

```json
{
    "error": "description of what went wrong"
}
```

| Status | Cause |
|--------|-------|
| `400` | Bad request (no file, invalid MIME type, file too large, validation error) |
| `403` | Access denied (missing or invalid token, access control denied) |
| `404` | Collection or document not found |
| `500` | Server error |

## Server Processing

When the server receives an upload:

1. **Validates** the MIME type against the collection's `mime_types` allowlist
2. **Checks** file size against `max_file_size`
3. **Sanitizes** the filename (lowercase, hyphens, unique prefix)
4. **Saves** the original file to `uploads/{collection}/{nanoid}_{filename}` (a random 10-character nanoid prefix, not the document ID)
5. **Resizes** images according to `image_sizes` (if configured)
6. **Generates** WebP/AVIF variants (if `format_options` configured)
7. **Runs** before-hooks within a transaction
8. **Creates** a document with all metadata fields populated
9. **Fires** after-hooks and publishes live events

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
  -H "Authorization: Bearer ${TOKEN}"
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
        crap.fields.text({ name = "title", required = true }),
        crap.fields.relationship({
            name = "cover_image",
            relationship = { collection = "media", has_many = false },
        }),
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

Upload API endpoints use Bearer token authentication:

```
Authorization: Bearer <jwt>
```

Obtain a token via the `Login` gRPC RPC or the admin login flow. Access control on the upload collection (`access.create`, `access.update`, `access.delete`) is enforced the same as for gRPC operations.
