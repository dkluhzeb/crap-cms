# Quick Start

## 1. Run with the example config

The repository includes an `example/` config directory with sample collections:

```bash
cargo run -- --config ./example
```

This starts:

- **Admin UI** at [http://localhost:3000/admin](http://localhost:3000/admin)
- **gRPC API** at `localhost:50051`

## 2. Bootstrap an admin user

The example config includes a `users` auth collection. Create the first user:

```bash
# Interactive (prompts for password)
cargo run -- --config ./example --create-user --email admin@example.com

# Non-interactive
cargo run -- --config ./example --create-user \
    --email admin@example.com \
    --password secret123 \
    --field role=admin \
    --field name="Admin User"
```

## 3. Log in to the admin UI

Visit [http://localhost:3000/admin/login](http://localhost:3000/admin/login) and sign in with the credentials you just created.

## 4. Create content via gRPC

Use `grpcurl` to interact with the API. The server supports reflection, so no proto import is needed:

```bash
# List all posts
grpcurl -plaintext localhost:50051 crap.ContentAPI/Find \
    -d '{"collection": "posts"}'

# Create a post
grpcurl -plaintext localhost:50051 crap.ContentAPI/Create \
    -d '{
      "collection": "posts",
      "data": {
        "title": "Hello World",
        "slug": "hello-world",
        "status": "draft",
        "content": "My first post."
      }
    }'
```

## 5. Create your own config

Start a new project by creating a config directory:

```bash
mkdir my-project
mkdir -p my-project/{collections,globals,hooks,templates,static,data}
```

Create `my-project/collections/posts.lua`:

```lua
crap.collections.define("posts", {
    labels = { singular = "Post", plural = "Posts" },
    fields = {
        { name = "title", type = "text", required = true },
        { name = "body", type = "richtext" },
    },
})
```

Run:

```bash
cargo run -- --config ./my-project
```
