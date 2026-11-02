# Oxploy — notas iniciais

Resumo das decisões tomadas antes de começar o projeto (2026-10-26).

## O que é

Um painel de deploy estilo [Dokploy](https://dokploy.com), mas **exclusivo para apps Rust** e bem
mais enxuto: cadastra um repo, faz o build, roda o binário e mostra tudo numa UI.

Nome: *oxide* + *deploy*. Livre no crates.io na época da escolha; GitHub e domínio
(`oxploy.dev`, `.sh`) ainda não foram checados.

## Decisões

- **Build release, não `cargo run`.** `cargo build --release` uma vez por deploy e roda o binário
  gerado: sobe na hora, não recompila no restart e permite guardar a versão anterior pra rollback.
- **Docker para isolar.** Build num container `rust:1.xx` com volumes de cache
  (`~/.cargo/registry` e `target/`); run do binário num container mínimo (`debian-slim` ou
  `distroless`). Dá isolamento entre apps e limite de CPU/RAM (`--memory`, `--cpus`).
- **Bancos como containers** (Postgres, Redis) com volumes nomeados, criados pela UI, e a
  `DATABASE_URL` injetada no app.
- **Controle do Docker via [`bollard`](https://crates.io/crates/bollard)**, falando com a API
  do Docker em vez de chamar o CLI.
- **Um app fullstack com o core isolado.** Um binário, um processo; a regra de negócio fica num
  módulo que não sabe nada de web. Detalhes em [Arquitetura](#arquitetura).

## Arquitetura

```
UI (topcoat) ──► Core (tokio) ──► bollard ──► Docker
                     │
                     ├─ SQLite (apps, envs, deploys)
                     ├─ git clone/pull (gix ou git CLI)
                     └─ Proxy reverso (Caddy) ──► domínio → container
```

### Não é front + back

O Topcoat já é fullstack: pages, shards e procedures rodam no servidor. Por isso **não existe
backend separado nem API HTTP interna**. Tudo é um binário e um processo, e a camada web chama o
core como função Rust comum:

```
┌──────────── 1 binário, 1 processo: oxploy ────────────┐
│                                                       │
│  topcoat (pages, shards, procedures)                  │
│        │  chamada de função normal, sem HTTP          │
│        ▼                                              │
│  core (lib): deploy(), logs(), create_volume()...     │
│        │                                              │
└────────┼──────────────────────────────────────────────┘
         ▼
   Docker / git / SQLite
```

```rust
#[procedure]
async fn deploy_app(cx: &Cx, app_id: u64) -> Result<()> {
    require_login(cx)?;              // coisa de UI/HTTP
    core::deploy(app_id).await?;     // coisa do motor
    Ok(())
}
```

A separação é só de organização de código, como *controllers* vs *services*:

- **`core`**: o motor. Deploy, Docker, git, SQLite, logs. É uma lib, não um app, e não importa
  nada do Topcoat. Não sabe se foi chamado por um botão, um CLI ou um teste.
- **`ui`**: a camada web. Telas, forms, login, e as "portas" (`#[procedure]`, `#[shard]`) que
  chamam o core. Usa `live!` + WebSocket para logs ao vivo e shards para status.

### Por que isolar o core

1. **Proteção contra o Topcoat.** Se ele quebrar ou for abandonado, reescreve-se só a `ui`
   (alternativa: axum + maud/askama + htmx).
2. **Várias portas de entrada.** UI, um CLI futuro (`oxploy deploy my-app`) e o webhook do GitHub
   usam o mesmo core, sem duplicar lógica.
3. **Testes.** O deploy é testável com `cargo test`, sem subir servidor nem browser.

### Layout: um crate só, por enquanto

```
src/
├── main.rs
├── core/   ← deploy, docker, db (sem nada de topcoat)
└── ui/     ← páginas topcoat
```

Regra: **nada em `core/` importa `topcoat`**. Quando surgir o segundo consumidor (o CLI), move-se
`core/` para um crate próprio (`oxploy-core`) num workspace.

## Escopo do MVP (v1)

1. Cadastrar app: repo git, branch, porta, variáveis de ambiente.
2. Deploy: clone → build no container com cache → run.
3. Logs ao vivo do build e do app.
4. Status e start / stop / restart.
5. Criar Postgres/Redis com volume e ligar ao app.

**Depois:** webhook do GitHub para auto-deploy, domínios com TLS automático (Caddy), deploy sem
downtime, multi-servidor, vários usuários.

## Riscos

- **Tempo de build.** É o problema nº 1. Cachear bem desde o início; avaliar
  [`cargo-chef`](https://github.com/LukeMathWalker/cargo-chef) para cachear dependências.
- **Segurança.** A UI controla o Docker, e acesso ao socket do Docker é praticamente root na
  máquina. Ter autenticação desde o primeiro dia, nem que seja uma senha.
- **Topcoat experimental.** Versão 0.9 na época, com breaking changes esperadas. Por isso o core
  é isolado.

## O que aprendemos com o Topcoat (demo em `~/Work/topcoat`)

- Exige **Rust 1.98+**. Instalado via asdf e fixado por projeto com `asdf set rust 1.98.1`
  (gera `.tool-versions`); o global continua 1.94.0. Usar asdf para gerenciar versões, não
  `rustup update`.
- Precisa do CLI: `cargo install topcoat-cli`. Depois rodar `asdf reshim rust 1.98.1` para o
  binário `topcoat` aparecer no PATH.
- Rodar com `topcoat dev`, não `cargo run`: é ele que gera o bundle de assets; sem o bundle o
  `AssetBundle::load()` falha.
- Para reatividade: `module_router!().assets(AssetBundle::load()?).discover().runtime().build()`
  e `topcoat::runtime::script()` no `<head>`.
- Recursos usados e que funcionaram: `signal` + `$(...)` (roda no browser), `#[shard]`
  (re-render no servidor), `#[procedure]` (RPC tipado), `asset!` para CSS.
- Shards e procedures viram endpoints públicos que **não passam pelos guards** de página e
  layout: autorizar e validar a entrada dentro de cada um.
- Runtime enviado ao browser na demo: ~11 KB gzip. Deve usar bem menos memória que um painel em
  Next.js, o que importa numa VPS pequena onde o painel divide RAM com os apps.

## Próximo passo

Montar o esqueleto: um crate com `src/core/` + `src/ui/`, Rust 1.98.1 fixado via asdf,
cadastro de app e deploy de um repo de exemplo.
