# Quick Start

## 1. Scaffold a new project

The fastest way to get started is the interactive `init` wizard:

```bash
crap-cms init ./my-project
```

The wizard walks you through:

1. **Admin port** (default: 3000)
2. **gRPC port** (default: 50051)
3. **Localization** — enable and choose locales (e.g., `en`, `de`, `fr`)
4. **Auth collection** — creates a `users` collection with email/password login
5. **First admin user** — prompts for email and password right away
6. **Upload collection** — creates a `media` collection for file/image uploads
7. **Additional collections** — keep adding as many as you need

A JWT auth secret is auto-generated and written to `crap.toml` so tokens survive restarts.

When it finishes you'll have a ready-to-run config directory:

```
my-project/
├── crap.toml
├── init.lua
├── .luarc.json
├── .gitignore
├── stylua.toml
├── collections/
│   ├── users.lua
│   └── media.lua
├── globals/
├── hooks/
├── access/
├── jobs/
├── plugins/
├── migrations/
├── templates/
├── static/
├── types/
│   └── crap.lua
├── data/
└── uploads/
```

## 2. Start the server

```bash
cd my-project
crap-cms serve
```

This starts:

- **Admin UI** at [http://localhost:3000/admin](http://localhost:3000/admin)
- **gRPC API** at `localhost:50051`

## 3. Log in to the admin UI

Visit [http://localhost:3000/admin/login](http://localhost:3000/admin/login) and sign in with the credentials you created during init.

If you skipped user creation during init, bootstrap one now:

```bash
# Interactive (prompts for password)
crap-cms user create -e admin@example.com

# Non-interactive
crap-cms user create \
    -e admin@example.com \
    -p secret123 \
    -f role=admin \
    -f name="Admin User"
```

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

## Alternative: Run with the example config

The repository includes an `example/` config directory with sample collections, useful if you're building from source:

```bash
git clone https://github.com/dkluhzeb/crap-cms.git
cd crap-cms
cargo build --release
./target/release/crap-cms serve -C ./example
```

Then bootstrap an admin user:

```bash
crap-cms user create -C ./example -e admin@example.com
```
