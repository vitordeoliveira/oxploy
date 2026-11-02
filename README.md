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
