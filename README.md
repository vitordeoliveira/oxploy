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
build script of your own that leaves the binary in `OXPLOY_OUTPUT_DIR`. A service can
instead deploy a file of its repository's latest GitHub release, built elsewhere, such as
in GitHub Actions: the binary itself, or a `.tar.gz` with it and the files it needs,
checked against the release's `.sha256` of the file when there is one. Such a service can
also deploy each new release on its own: oxploy looks at the repository every minute, and
does not try again a release that failed. A binary built for another machine, such as a
Linux one on a Mac, fails its deployment before the running release stops. Each app
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
Caddy also sends oxploy the requests an app answered with a server error, at
`127.0.0.1:2020` unless `OXPLOY_CADDY_LOGS` says otherwise, and oxploy writes them in the
app's log: an app may answer 500 without printing why, and Caddy answers 502 when the app
does not answer.
A service without a domain the first time it goes live gets one at the server's public
IPv4 address through [sslip.io](https://sslip.io), such as
`shop-api.203-0-113-7.sslip.io`, over HTTPS, so it opens without a DNS record. oxploy
gives it only that once, so deleting it is for good. Let's Encrypt's limits for sslip.io
are shared by everyone who uses it; if a certificate does not come, switch the domain to
plain HTTP or add one of your own. The panel itself gets a domain under **Settings**,
always over HTTPS; until then it answers only at `HOST` and `PORT`, `127.0.0.1:3000` by
default. On macOS, `brew services start caddy` needs a Caddyfile at
`/opt/homebrew/etc/Caddyfile`, and one with only the admin API in it is enough. Caddy
takes a block's braces only at the end and the start of their lines, so it refuses
`{ admin localhost:2019 }` on one line:

```
{
	admin localhost:2019
}
```

## Running on a server

Each [release](https://github.com/vitordeoliveira/oxploy/releases) has
`oxploy-x86_64-linux.tar.gz`: the binary and the `assets` folder it loads, built on Ubuntu
22.04. Pushing a tag such as `v0.2.0` that matches the version in `Cargo.toml` builds and
publishes one. The server also needs `git`, Rust and a C compiler to build the apps, and
Caddy for their domains. To install, giving the folders to the user the service below
runs as, so that the panel can update itself:

```sh
mkdir -p /opt/oxploy /var/lib/oxploy
curl -L https://github.com/vitordeoliveira/oxploy/releases/latest/download/oxploy-x86_64-linux.tar.gz | tar xz -C /opt/oxploy
chown -R oxploy: /opt/oxploy /var/lib/oxploy
```

Run it as a systemd service, as a user whose `PATH` has the Rust that builds the apps.
`KillMode=process` keeps the apps running while oxploy stops or restarts:

```ini
[Service]
User=oxploy
WorkingDirectory=/var/lib/oxploy
ExecStart=/opt/oxploy/oxploy
Environment=DATABASE_URL=sqlite:/var/lib/oxploy/oxploy.db
Environment=OXPLOY_DATA_DIR=/var/lib/oxploy/data
Environment=PATH=/var/lib/oxploy/.cargo/bin:/usr/local/bin:/usr/bin:/bin
Restart=always
KillMode=process
```

The panel listens on `127.0.0.1:3000`, and Caddy serves it at `https://` and the
server's public IPv4 address, with or without a domain: Let's Encrypt certifies the
address for six days at a time, and Caddy renews it, or signs one itself, which browsers
warn about, if that fails. Behind NAT, where the machine does not see its public address,
set `OXPLOY_PUBLIC_IP`. On the first run, oxploy writes a setup code to its log and to
`setup-code` in its data folder, such as `/var/lib/oxploy/data/setup-code`: creating the
super admin asks for it, so whoever opens the address first cannot take the server.

oxploy looks for a new release on GitHub every hour, and shows the super admin **Update
available** at the foot of the sidebar once one is out; **Settings** checks on demand.
Updating downloads the release's build, checks it against its checksum and that it runs on
the machine, and puts it in place of the binary and the `assets` folder, which it keeps in
`previous` beside them. Then the panel restarts on it, with the same process ID, once the
deployments in progress finish. The apps keep running. It needs the panel's user to own the
folder it is installed in. To go back, stop oxploy, move `previous/oxploy` and
`previous/assets` back in place, and start it.

**Settings** also picks the time zone the panel shows times in: its clock, its charts and
dates, and the times oxploy writes in the apps' logs and backups. Until one is picked, it
is the server's own, and the apps keep the server's own either way.

Once a day, oxploy tells oxploy.com that it runs, with `POST /api/ping`: its version, the
machine, such as `x86_64-linux`, and a random ID it made the first time it started, which
tells its reports apart and says nothing about the server. The super admin can also share
what the server runs, in numbers only, such as how many apps, domains and deployments it
has, never their names. **Settings** turns either off, shows the report as it would go
now, and when the last one went out. `OXPLOY_TELEMETRY=off` or `DO_NOT_TRACK=1` turn it off
whatever Settings says. A development build does not report, unless
`OXPLOY_TELEMETRY_URL` sends its reports elsewhere, such as to the site running locally at
`http://127.0.0.1:3000/api/ping`. The code is in `src/features/telemetry.rs`, and what the
site keeps is on [its documentation](https://oxploy.com/docs/usage-reporting).

## MCP

`/mcp` lets AI agents, such as a bot or Claude Code, reach the server and its apps through
the [Model Context Protocol](https://modelcontextprotocol.io). Each client gets a token of
its own on the **MCP** page, which shows it once: oxploy keeps only its hash. Clients send
it as `Authorization: Bearer <token>`, and a missing or wrong one gets a 401 after a
second. Revoking a token on the same page turns its client away. A client that needs
both kinds below holds one token of each.

An organization's token reaches the tools of its apps that serve MCP, the ones turned on
in each app's **MCP** tab, as described below. Each is named after its app, such as
`porta-global__list_requests`, and a call goes to the app's own server. An app that is
not running lists none. Other organizations' tokens see none of them.

Each of the organization's tokens reaches all the tools turned on, including the ones
turned on later, or only the ones picked for it. A new token starts with the ones picked
in the dialog that makes it, none unless some are; its **Tools** button changes them
later, and its row warns when it reaches no tool. Turning a tool off in the app's tab
takes it from every token, and turning it back on gives it back to the tokens that had
it picked. A call to a tool the token does not reach gets the same error as a tool that
does not exist, and each call an organization's token makes is logged with the token,
the tool, how it went and how long it took, but not its arguments. Tokens made before
tools could be picked reach all of them.

An organization's token can be **available to this organization's apps**, for a client
that runs here as one of them, such as a bot. oxploy then keeps the token itself, not
only its hash, and an app takes it as a variable named after it, `${OXPLOY_MCP_<NAME>}`,
such as `MCP_TOKEN=${OXPLOY_MCP_TELEGRAM_BOT}` in its Environment tab, which the
token's row shows. Only an app whose variables use it gets it, saving checks that the
token exists, and one revoked since is empty, without the app losing its other
variables. A token made before, whose secret oxploy did not keep, is made available with
**For apps**, which gives it a new one. Every app also gets `OXPLOY_MCP_URL`, where this
server's MCP answers on 127.0.0.1. Server tokens are never available to apps.

A server token, which only the super admin makes, reads the machine, oxploy itself and
every app it runs, across organizations, with these tools:

| Tool | What it returns |
| --- | --- |
| `server_status` | The machine's CPU, memory, disk and network; oxploy's version, updates and the one being installed, and Caddy; how many apps stand where |
| `list_services` | Every app, with its status, port, domains, release, usage now, crashes and latest deployment |
| `service_usage` | One app's process, its CPU and memory now and over the last 10 minutes, its crashes and its storage |
| `app_logs` | The end of an app's log, or only its lines with a text |
| `list_deployments` | An app's latest deployments |
| `deployment_log` | The end of a deployment's log |
| `list_mcp_tokens` | The server's tokens and each organization's, with the tools turned on in its apps and the ones each token reaches, never their secrets |
| `app_mcp_tools` | The tools an app's MCP server offers, what each declares it does, and which are turned on |

A server token made with **Can manage** on also gets the tools that change something,
and its row says so. Clients such as Claude Code ask before each call unless told not
to, and each call is logged with the token, the tool, how it went and how long it took:

| Tool | What it does |
| --- | --- |
| `update_oxploy` | Installs oxploy's newest release, as the Update button does, and answers at once |
| `set_app_tools` | Turns tools of an app's MCP server on or off, as its MCP tab does |
| `set_token_tools` | Has an organization's token reach every tool turned on, or only the ones given |
| `create_mcp_token` | Makes an organization's token with the tools picked, and returns its secret and the `claude mcp add` commands once |
| `revoke_mcp_token` | Revokes an organization's token |

Server tokens are only made and revoked on the page, so an agent cannot make itself a
token that manages, and the tools that manage reach only the organizations' tokens. A
token that only reads gets the same error for them as for a tool that does not exist.
Server tokens made before tokens could manage only read.

With a new token, the page gives the commands that connect Claude Code to it, under a
name made from the token's: `oxploy-server-<token>` for a server token, or
`oxploy-<organization>-<token>` for an organization's, so a client that holds several
tells them apart. One adds it only in the project Claude Code runs in, with
`--scope local`, and the other in every project of its user, with `--scope user`; both
keep it in `~/.claude.json`. Neither uses `--scope project`, which would write the token
into the repository's `.mcp.json`. Run them in a terminal of your own: in a Claude
conversation, the token would stay in its history. For a server token named Claude
Local:

```sh
claude mcp add --transport http --scope local oxploy-server-claude-local https://<panel>/mcp \
  --header "Authorization: Bearer <token>"
```

Two tokens of the server, or of one organization, cannot have names that read the same,
such as `Claude Local` and `claude-local`. Making a token sends the browser on to the page,
which shows it once, so reloading the page makes no second token.

`src/app/mcp.rs` answers each JSON-RPC message, sent by POST, with plain JSON, with no
session and no event stream. Each server tool is a file of its own in `src/app/mcp/`, with
a line each in `definitions` and `call_tool`, and `src/app/mcp/apps.rs` passes on the
apps' tools.

An app can serve an MCP server of its own, over Streamable HTTP on its port. Its **MCP**
tab turns that on at a path, `/mcp` unless set otherwise. oxploy then gives the app
`OXPLOY_MCP_TOKEN` when it starts, calls its server at 127.0.0.1 with that token as
`Authorization: Bearer <token>`, and has Caddy answer 404 at the path on the app's
domains, so only oxploy reaches it. An app that reads another variable takes the token
with a line such as `MCP_TOKEN=${OXPLOY_MCP_TOKEN}` in its Environment tab. The tab lists
the app's tools, each off until it is turned on, and the organization's tokens reach the
ones turned on, all of them or the ones picked for each token. The tab and a token's
tools group them by what the app declares they do to its data: read-only, writes, or
destructive, where a tool that declares nothing counts as destructive, as the protocol
says. `src/features/app_mcp.rs` is the client, which takes answers in plain JSON
or as an event stream, with or without a session.

An MCP server that runs elsewhere, such as one of an app hosted on another platform,
joins an organization as a service of its own kind: **New service**, then **External
MCP server**, with the URL it answers at, such as `https://api.example.com/mcp`, and the
token it takes as `Authorization: Bearer <token>`. Left empty, oxploy makes one, to put
in the server's configuration. Its page is its MCP tab alone, with the URL and the token,
and its tools are turned on and picked for tokens as an app's are. oxploy calls it over
HTTPS only, follows no redirect, which would carry the token elsewhere, and never calls
an address of its own machine or of a private network, whatever the URL's host resolves
to. A server that cannot be reached lists no tools until it can. Servers that need a
sign-in, such as OAuth, are not supported.

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
