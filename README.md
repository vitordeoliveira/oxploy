# oxploy

A [Topcoat](https://github.com/tokio-rs/topcoat) application.

## Development

Install the Topcoat CLI:

```sh
cargo install topcoat-cli
```

The database is stored in `data.db`; set `DATABASE_URL` to use another one. The app
applies pending migrations from `toasty/` when it starts. After changing a model,
generate a migration for it:

```sh
cargo run -- toasty migration generate --name <what_changed>
```

Start the development server, which rebuilds the application when you edit its source:

```sh
topcoat dev
```

Then open <http://127.0.0.1:3000>. On a fresh database it asks you to create the super
admin; after that it shows the sign-in page.

The session cookie is `Secure`, which browsers accept over plain HTTP only on
`localhost` and `127.0.0.1`. Anywhere else, serve the panel over HTTPS, or signing in
will not stick.

Services build from GitHub. Open **Git** in the sidebar to connect an account: oxploy
registers a private GitHub App on it that can only read code. The app sends the browser
back to the address the panel was opened at, so connect from the address you will keep
using.

Deploying runs `git` and `cargo` from the panel's `PATH`, on the machine itself, or a
build script of your own that leaves the binary in `OXPLOY_OUTPUT_DIR`. Each app
runs as a process listening on `127.0.0.1`, at the port oxploy passes in `PORT`. Apps keep
running when the panel stops or restarts, and the panel takes them back when it starts.
Their code, builds and logs live in `data/`; set `OXPLOY_DATA_DIR` to keep them
elsewhere. Each app also gets a storage folder that deploys keep, in
`OXPLOY_STORAGE_DIR`, and can run a script after each build, before the new release
starts, such as one that migrates its database. The Storage tab adds SQLite databases
there and connects them to the app through a variable, a link in each release, or
both, and backs them up and restores them. Each service's Monitoring tab shows the CPU,
memory and disk its app takes, and the server's Monitoring page shows the machine's and,
in its oxploy tab, the panel's and its builds', all sampled every 2 seconds and kept for
10 minutes in memory.

Domains go through [Caddy](https://caddyserver.com), which runs on the same machine as a
service of its own, such as the one its packages install. The Caddy belongs to oxploy:
oxploy builds every route from the database and loads them through Caddy's admin API, at
`127.0.0.1:2019` unless `OXPLOY_CADDY_ADMIN` says otherwise, and loads them again whenever
Caddy loses them, as after a restart. Caddy serves ports 80 and 443 (`OXPLOY_HTTP_PORT` and
`OXPLOY_HTTPS_PORT` change them) and gets each domain's certificate from Let's Encrypt.
On macOS, `brew services start caddy` needs a Caddyfile at `/opt/homebrew/etc/Caddyfile`,
and one with only `{ admin localhost:2019 }` in it is enough.

## Project structure

| Path | Contents |
| --- | --- |
| `src/app.rs`, `src/app/` | The route tree: the router, layouts, pages, and routes. |
| `src/features.rs`, `src/features/` | Everything one feature owns, such as its data and business rules. |
| `src/components.rs`, `src/components/` | Shared UI components that belong to no single feature. |
| `src/db.rs` | The database connection. |
| `toasty/` | The database migrations. Commit them with your code. |

Pages in `app` use features and components. A feature can use shared components but never imports from `app`, and shared components never import from a feature.

## Learn more

- [Getting started](https://github.com/tokio-rs/topcoat/blob/main/docs/getting_started.md)
- [Guides](https://github.com/tokio-rs/topcoat/tree/main#learn-topcoat)
