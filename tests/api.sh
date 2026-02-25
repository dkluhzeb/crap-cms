#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────
# Crap CMS — gRPC API test requests (grpcurl)
#
# Requires: grpcurl (https://github.com/fullstorydev/grpcurl)
# Server:   localhost:50051 (default grpc_port in crap.toml)
# Uses server reflection — no proto import needed.
#
# Collections used: "posts" and "pages" (from example/collections/)
#
# Note: Hooks now have local API access. before_change / before_delete hooks
# can call crap.collections.find(), .create(), .update(), .delete() and those
# calls share the parent operation's transaction (full atomicity).
#
# Usage:
#   Run individual commands by copying them, or source this file
#   and call the functions:
#     source tests/api.sh
#     find_posts
#     create_post
# ──────────────────────────────────────────────────────────────

ADDR="localhost:50051"

# ── Find ──────────────────────────────────────────────────────

# List all posts (no filters)
find_posts() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "depth": "1"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with pagination
find_posts_paginated() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "limit": "10",
  "offset": "0"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts filtered by status
find_posts_published() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "filters": { "status": "published" }
}' "$ADDR" crap.ContentAPI/Find
}

# List posts ordered by title
find_posts_ordered() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "order_by": "title"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with advanced filter (where clause)
find_posts_where() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"equals\":\"published\"},\"title\":{\"contains\":\"hello\"}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with in operator
find_posts_in() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"in\":[\"draft\",\"published\"]}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with not_equals operator
find_posts_not_equals() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"not_equals\":\"draft\"}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with like operator
find_posts_like() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"title\":{\"like\":\"Hello%\"}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with greater_than operator
find_posts_greater_than() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"created_at\":{\"greater_than\":\"2024-01-01\"}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with less_than operator
find_posts_less_than() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"created_at\":{\"less_than\":\"2099-01-01\"}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with not_in operator
find_posts_not_in() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"not_in\":[\"archived\"]}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with exists operator
find_posts_exists() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"title\":{\"exists\":true}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with not_exists operator
find_posts_not_exists() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"not_exists\":true}}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with OR filter (title contains "hello" OR status = "draft")
find_posts_or() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"status\":\"draft\"}]}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with OR + AND: status = "published" AND (title contains "hello" OR title contains "world")
find_posts_or_with_and() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":\"published\",\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"title\":{\"contains\":\"world\"}}]}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with OR multi-condition groups: (status = "published" AND title contains "hello") OR (status = "draft")
find_posts_or_multi() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"or\":[{\"status\":\"published\",\"title\":{\"contains\":\"hello\"}},{\"status\":\"draft\"}]}"
}' "$ADDR" crap.ContentAPI/Find
}

# List posts with field selection (only title and status)
find_posts_select() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "select": ["title", "status"]
}' "$ADDR" crap.ContentAPI/Find
}

# Find a post by ID with field selection
find_post_by_id_select() {
  local id="${1:?Usage: find_post_by_id_select <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"select\": [\"title\", \"status\"]
  }" "$ADDR" crap.ContentAPI/FindByID
}

# List all pages
find_pages() {
grpcurl -plaintext -d '{
  "collection": "pages"
}' "$ADDR" crap.ContentAPI/Find
}

# ── FindByID ──────────────────────────────────────────────────

# Get a single post by ID
find_post_by_id() {
  local id="${1:?Usage: find_post_by_id <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/FindByID
}

# Get a single page by ID
find_page_by_id() {
  local id="${1:?Usage: find_page_by_id <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"pages\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/FindByID
}

# ── Create ────────────────────────────────────────────────────

# Create a new draft post
create_post() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "data": {
    "title": "Hello World",
    "slug": "hello-world",
    "status": "draft",
    "content": "This is my first post."
  }
}' "$ADDR" crap.ContentAPI/Create
}

# Create a published post
create_post_published() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "data": {
    "title": "Second Post",
    "slug": "second-post",
    "status": "published",
    "content": "Published from the gRPC API."
  }
}' "$ADDR" crap.ContentAPI/Create
}

# Create a new page
create_page() {
grpcurl -plaintext -d '{
  "collection": "pages",
  "data": {
    "title": "About",
    "slug": "about",
    "body": "This is the about page.",
    "published": "true"
  }
}' "$ADDR" crap.ContentAPI/Create
}

# ── Tags (for relationship testing) ──────────────────────────

# Create a tag
create_tag() {
grpcurl -plaintext -d '{
  "collection": "tags",
  "data": {
    "name": "rust",
    "color": "#ff6600"
  }
}' "$ADDR" crap.ContentAPI/Create
}

# List all tags
find_tags() {
grpcurl -plaintext -d '{
  "collection": "tags"
}' "$ADDR" crap.ContentAPI/Find
}

# ── New field types ──────────────────────────────────────────

# Create a post with has-many tags (array of tag IDs) and array (slides) fields
create_post_with_relations() {
  local tag1="${1:?Usage: create_post_with_relations <tag_id_1> <tag_id_2>}"
  local tag2="${2:?Usage: create_post_with_relations <tag_id_1> <tag_id_2>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"data\": {
      \"title\": \"Post With Relations\",
      \"slug\": \"post-with-relations\",
      \"status\": \"draft\",
      \"content\": \"<p>Richtext content here.</p>\",
      \"tags\": [\"$tag1\", \"$tag2\"],
      \"slides\": [
        {\"title\": \"Slide 1\", \"image_url\": \"https://example.com/1.jpg\", \"caption\": \"First slide\"},
        {\"title\": \"Slide 2\", \"image_url\": \"https://example.com/2.jpg\", \"caption\": \"Second slide\"}
      ]
    }
  }" "$ADDR" crap.ContentAPI/Create
}

# Find a post by ID (returns hydrated relationship and array data, depth=1 default)
find_post_hydrated() {
  local id="${1:?Usage: find_post_hydrated <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/FindByID
}

# Find posts with depth=1 (populate immediate relationships)
find_posts_depth() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "depth": 1
}' "$ADDR" crap.ContentAPI/Find
}

# Find a post by ID with depth=2 (populate nested relationships)
find_post_depth2() {
  local id="${1:?Usage: find_post_depth2 <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"depth\": 2
  }" "$ADDR" crap.ContentAPI/FindByID
}

# Find a post by ID with depth=0 (IDs only, no population)
find_post_depth0() {
  local id="${1:?Usage: find_post_depth0 <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"depth\": 0
  }" "$ADDR" crap.ContentAPI/FindByID
}

# ── Update ────────────────────────────────────────────────────

# Update a post
update_post() {
  local id="${1:?Usage: update_post <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"data\": {
      \"title\": \"Hello World (Updated)\",
      \"status\": \"published\"
    }
  }" "$ADDR" crap.ContentAPI/Update
}

# Update a page
update_page() {
  local id="${1:?Usage: update_page <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"pages\",
    \"id\": \"$id\",
    \"data\": {
      \"title\": \"About Us\",
      \"body\": \"Updated about page content.\"
    }
  }" "$ADDR" crap.ContentAPI/Update
}

# ── Delete ────────────────────────────────────────────────────

# Delete a post
delete_post() {
  local id="${1:?Usage: delete_post <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/Delete
}

# Delete a page
delete_page() {
  local id="${1:?Usage: delete_page <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"pages\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/Delete
}

# ── Globals ───────────────────────────────────────────────────

# Get site settings global
get_site_settings() {
  grpcurl -plaintext -d '{"slug": "site_settings"}' "$ADDR" crap.ContentAPI/GetGlobal
}

# Update site settings global
update_site_settings() {
  grpcurl -plaintext -d '{
    "slug": "site_settings",
    "data": {"site_name": "My Updated Site", "tagline": "A CMS"}
  }' "$ADDR" crap.ContentAPI/UpdateGlobal
}

# ── Auth ─────────────────────────────────────────────────────

# CLI user create (alternative to gRPC, good for bootstrapping first admin):
#   crap-cms user create ./example -e admin@example.com
#   crap-cms user create ./example -e admin@example.com \
#       -p secret123 -f role=admin -f name="Admin User"

# Create a user (via the standard Create RPC — password in data)
create_user() {
grpcurl -plaintext -d '{
  "collection": "users",
  "data": {
    "email": "admin@example.com",
    "password": "secret123",
    "name": "Admin User",
    "role": "admin"
  }
}' "$ADDR" crap.ContentAPI/Create
}

# Login as a user
login_users() {
grpcurl -plaintext -d '{
  "collection": "users",
  "email": "admin@example.com",
  "password": "secret123"
}' "$ADDR" crap.ContentAPI/Login
}

# Get current user from token
me() {
  local token="${1:?Usage: me <token>}"
  grpcurl -plaintext -d "{
    \"token\": \"$token\"
  }" "$ADDR" crap.ContentAPI/Me
}

# ── Password Reset & Email Verification ──────────────────────

# Forgot password (always returns success — doesn't leak user existence)
forgot_password() {
grpcurl -plaintext -d '{
  "collection": "users",
  "email": "admin@example.com"
}' "$ADDR" crap.ContentAPI/ForgotPassword
}

# Reset password with token from email
reset_password() {
  local token="${1:?Usage: reset_password <token>}"
  grpcurl -plaintext -d "{
    \"collection\": \"users\",
    \"token\": \"$token\",
    \"new_password\": \"newsecret123\"
  }" "$ADDR" crap.ContentAPI/ResetPassword
}

# Verify email with token from email
verify_email() {
  local token="${1:?Usage: verify_email <token>}"
  grpcurl -plaintext -d "{
    \"collection\": \"users\",
    \"token\": \"$token\"
  }" "$ADDR" crap.ContentAPI/VerifyEmail
}

# ── Authenticated requests ────────────────────────────────────

# Find posts with Bearer token (for access-controlled collections)
find_posts_authed() {
  local token="${1:?Usage: find_posts_authed <token>}"
  grpcurl -plaintext -H "authorization: Bearer $token" -d '{
    "collection": "posts"
  }' "$ADDR" crap.ContentAPI/Find
}

# Create a post with Bearer token
create_post_authed() {
  local token="${1:?Usage: create_post_authed <token>}"
  grpcurl -plaintext -H "authorization: Bearer $token" -d '{
    "collection": "posts",
    "data": {
      "title": "Authenticated Post",
      "slug": "auth-post",
      "status": "draft",
      "content": "Created with auth token."
    }
  }' "$ADDR" crap.ContentAPI/Create
}

# Delete a post with Bearer token
delete_post_authed() {
  local token="${1:?Usage: delete_post_authed <token> <id>}"
  local id="${2:?Usage: delete_post_authed <token> <id>}"
  grpcurl -plaintext -H "authorization: Bearer $token" -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/Delete
}

# ── Schema Discovery ─────────────────────────────────────────

# List all collections and globals (lightweight overview)
list_collections() {
  grpcurl -plaintext -d '{}' "$ADDR" crap.ContentAPI/ListCollections
}

# Describe a collection's full field schema
describe_collection() {
  local slug="${1:?Usage: describe_collection <slug> [--global]}"
  local is_global="false"
  if [[ "${2:-}" == "--global" ]]; then
    is_global="true"
  fi
  grpcurl -plaintext -d "{
    \"slug\": \"$slug\",
    \"is_global\": $is_global
  }" "$ADDR" crap.ContentAPI/DescribeCollection
}

# ── Reflection / Discovery ────────────────────────────────────

# List all available services
list_services() {
  grpcurl -plaintext "$ADDR" list
}

# Describe the ContentAPI service
describe_api() {
  grpcurl -plaintext "$ADDR" describe crap.ContentAPI
}

# Describe a specific message type
describe_message() {
  local msg="${1:?Usage: describe_message <type> (e.g. crap.FindRequest)}"
  grpcurl -plaintext "$ADDR" describe "$msg"
}

# ── Upload Collections (media) ──────────────────────────────

# Describe the media upload collection (verify upload fields in schema)
describe_media() {
  grpcurl -plaintext -d '{ "slug": "media" }' \
    "$ADDR" crap.ContentAPI/DescribeCollection
}

# List media items
find_media() {
  grpcurl -plaintext -d '{ "collection": "media" }' \
    "$ADDR" crap.ContentAPI/Find
}

# Create a media document (metadata only — file upload via admin UI)
# Note: Upload collections are primarily used via the admin UI for file upload.
# Via gRPC, clients can create/update metadata directly.
create_media_metadata() {
  grpcurl -plaintext -d '{
    "collection": "media",
    "data": {
      "filename": "test-image.png",
      "mime_type": "image/png",
      "filesize": "12345",
      "url": "/uploads/media/abc_test-image.png",
      "alt": "Test image"
    }
  }' "$ADDR" crap.ContentAPI/Create
}

# ── Group / Upload / Blocks field types ──────────────────────

# Create a document with group field data (flat prefixed keys)
create_with_group() {
  grpcurl -plaintext -d '{
    "collection": "pages",
    "data": {
      "title": "Page With SEO",
      "seo__title": "My Custom SEO Title",
      "seo__description": "SEO description here"
    }
  }' "$ADDR" crap.ContentAPI/Create
}

# Create a document with blocks field data
create_with_blocks() {
  grpcurl -plaintext -d '{
    "collection": "pages",
    "data": {
      "title": "Page With Blocks",
      "content": [
        { "_block_type": "hero", "heading": "Welcome", "subheading": "To our site" },
        { "_block_type": "richtext", "body": "<p>Some rich text content here.</p>" }
      ]
    }
  }' "$ADDR" crap.ContentAPI/Create
}

# Create a document with an upload field reference
create_with_upload() {
  local media_id="${1:?Usage: create_with_upload <media_id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"data\": {
      \"title\": \"Post With Upload\",
      \"featured_image\": \"$media_id\"
    }
  }" "$ADDR" crap.ContentAPI/Create
}

# ── Locale ───────────────────────────────────────────────────

# Find posts with a specific locale (returns flat field names for that locale)
find_posts_locale() {
  local locale="${1:?Usage: find_posts_locale <locale> (e.g., en, de, all)}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"locale\": \"$locale\"
  }" "$ADDR" crap.ContentAPI/Find
}

# Find a post by ID with locale
find_post_by_id_locale() {
  local id="${1:?Usage: find_post_by_id_locale <id> <locale>}"
  local locale="${2:?Usage: find_post_by_id_locale <id> <locale>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"locale\": \"$locale\"
  }" "$ADDR" crap.ContentAPI/FindByID
}

# Create a post in a specific locale
create_post_locale() {
  local locale="${1:?Usage: create_post_locale <locale>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"locale\": \"$locale\",
    \"data\": {
      \"title\": \"Hallo Welt\",
      \"slug\": \"hallo-welt\",
      \"status\": \"draft\",
      \"content\": \"Dies ist mein erster Beitrag.\"
    }
  }" "$ADDR" crap.ContentAPI/Create
}

# Update a post in a specific locale
update_post_locale() {
  local id="${1:?Usage: update_post_locale <id> <locale>}"
  local locale="${2:?Usage: update_post_locale <id> <locale>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"locale\": \"$locale\",
    \"data\": {
      \"title\": \"Hallo Welt (Aktualisiert)\"
    }
  }" "$ADDR" crap.ContentAPI/Update
}

# Get a global with locale
get_site_settings_locale() {
  local locale="${1:?Usage: get_site_settings_locale <locale>}"
  grpcurl -plaintext -d "{
    \"slug\": \"site_settings\",
    \"locale\": \"$locale\"
  }" "$ADDR" crap.ContentAPI/GetGlobal
}

# Update a global in a specific locale
update_site_settings_locale() {
  local locale="${1:?Usage: update_site_settings_locale <locale>}"
  grpcurl -plaintext -d "{
    \"slug\": \"site_settings\",
    \"locale\": \"$locale\",
    \"data\": {\"site_name\": \"Meine Seite\", \"tagline\": \"Ein CMS\"}
  }" "$ADDR" crap.ContentAPI/UpdateGlobal
}

# ── Subscribe (Live Updates) ──────────────────────────────────

# Subscribe to all mutation events (streams indefinitely)
subscribe_all() {
  grpcurl -plaintext -d '{}' "$ADDR" crap.ContentAPI/Subscribe
}

# Subscribe to specific collections
subscribe_posts() {
  grpcurl -plaintext -d '{
    "collections": ["posts"]
  }' "$ADDR" crap.ContentAPI/Subscribe
}

# Subscribe with auth and operation filter
subscribe_auth() {
  local token="${1:?Usage: subscribe_auth <token>}"
  grpcurl -plaintext -d "{
    \"collections\": [\"posts\"],
    \"operations\": [\"create\", \"update\"],
    \"token\": \"$token\"
  }" "$ADDR" crap.ContentAPI/Subscribe
}

# ── Versioning & Drafts ──────────────────────────────────────

# Create a post as draft (requires collection with versions.drafts = true)
create_post_draft() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "draft": true,
  "data": {
    "title": "Draft Post",
    "slug": "draft-post",
    "content": "This is a draft."
  }
}' "$ADDR" crap.ContentAPI/Create
}

# Update a post as draft (version-only save, main table unchanged)
update_post_draft() {
  local id="${1:?Usage: update_post_draft <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"draft\": true,
    \"data\": {
      \"title\": \"Updated Draft Title\"
    }
  }" "$ADDR" crap.ContentAPI/Update
}

# Find posts including drafts (draft=true skips _status=published filter)
find_posts_with_drafts() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "draft": true
}' "$ADDR" crap.ContentAPI/Find
}

# Find by ID loading latest draft version
find_post_draft_version() {
  local id="${1:?Usage: find_post_draft_version <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\",
    \"draft\": true
  }" "$ADDR" crap.ContentAPI/FindByID
}

# List version history for a document
list_versions() {
  local id="${1:?Usage: list_versions <id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"id\": \"$id\"
  }" "$ADDR" crap.ContentAPI/ListVersions
}

# Restore a specific version
restore_version() {
  local doc_id="${1:?Usage: restore_version <document_id> <version_id>}"
  local version_id="${2:?Usage: restore_version <document_id> <version_id>}"
  grpcurl -plaintext -d "{
    \"collection\": \"posts\",
    \"document_id\": \"$doc_id\",
    \"version_id\": \"$version_id\"
  }" "$ADDR" crap.ContentAPI/RestoreVersion
}

# ── Count / Bulk Operations ─────────────────────────────────

# Count all posts
count_posts() {
grpcurl -plaintext -d '{
  "collection": "posts"
}' "$ADDR" crap.ContentAPI/Count
}

# Count posts with filter
count_posts_published() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "filters": { "status": "published" }
}' "$ADDR" crap.ContentAPI/Count
}

# Count posts with where clause
count_posts_where() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"not_equals\":\"archived\"}}"
}' "$ADDR" crap.ContentAPI/Count
}

# Update many posts (bulk update)
update_many_posts() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "filters": { "status": "draft" },
  "data": { "status": "published" }
}' "$ADDR" crap.ContentAPI/UpdateMany
}

# Update many with where clause
update_many_posts_where() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"equals\":\"draft\"}}",
  "data": { "status": "published" }
}' "$ADDR" crap.ContentAPI/UpdateMany
}

# Delete many posts (bulk delete)
delete_many_posts() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "filters": { "status": "archived" }
}' "$ADDR" crap.ContentAPI/DeleteMany
}

# Delete many with where clause
delete_many_posts_where() {
grpcurl -plaintext -d '{
  "collection": "posts",
  "where": "{\"status\":{\"equals\":\"archived\"}}"
}' "$ADDR" crap.ContentAPI/DeleteMany
}
