# Backlog

Ideias decididas para depois. Cada item diz o problema, o que já sabemos e o que falta decidir.

## Topcoat

### Bundle de assets para qualquer versão do Topcoat

**Problema.** No build Cargo, o oxploy gera a pasta `assets` dos apps Topcoat com o bundler
do `topcoat-asset` que ele traz embutido (hoje 0.11). O formato da etiqueta que o `asset!()`
grava no binário é dessa versão. Um app numa linha incompatível (0.12, por exemplo) recebe
só um aviso no log do deploy, e pode quebrar ao subir se o formato tiver mudado.

**Objetivo.** Funcionar com qualquer versão do Topcoat que o app usar.

**Caminhos já discutidos:**

- **Lista de versões.** O oxploy depende de várias versões do `topcoat-asset` com nomes
  diferentes (`topcoat-asset-011`, `topcoat-asset-012`) e escolhe pela versão no
  `Cargo.lock` do app, que ele já lê. Continua sem precisar do CLI e aceitando
  `--features`, mas cada versão nova do Topcoat é uma dependência a mais.
- **CLI sob demanda.** Na primeira vez que aparece uma versão, o oxploy instala o
  `topcoat-cli` dela (`cargo install topcoat-cli@X --locked`) numa pasta própria e roda
  `topcoat asset bundle`. Funciona com qualquer versão, mas o primeiro deploy fica mais
  lento, precisa de internet e o `topcoat asset bundle` não aceita `--features`.

### Build type "Topcoat"

**Em aberto.** Uma aba "Topcoat" ao lado de "Cargo" e "Script" nas opções de build, para
deixar explícito que o app é Topcoat, em vez de o oxploy detectar pelo `Cargo.lock`.
Abriria espaço para opções próprias do Topcoat, como servir os assets de um CDN.

**A decidir:**

- se a aba "Cargo" continua gerando o bundle sozinha, ou só avisa que o app parece Topcoat;
- como ela se relaciona com o suporte a qualquer versão, acima.

## Monitoring

### Tráfego de rede por app

**Problema.** A aba Monitoring de cada serviço mostra CPU, memória e disco, mas não rede.
Rodando direto na máquina, o app usa a rede da própria máquina, e o sistema conta bytes por
interface, não por processo. O Docker consegue contar por container porque cada um tem a
sua própria interface. A página Monitoring do servidor mostra a rede, mas da máquina inteira.

**Objetivo.** Ver, por app, quanto tráfego ele recebe e envia, e de preferência como as
requisições vão: quantas, quantas com erro e em quanto tempo respondem.

**Caminhos já discutidos:**

- **Pelo Caddy (o preferido).** Quando os domínios entrarem, todo o tráfego de um app passa
  pelo Caddy, que já mede por host: requisições, bytes, status e latência. O oxploy lê isso
  e mostra na aba. Para um app web, é mais útil do que bytes de rede. Fica de fora o
  tráfego que o app faz por conta própria, como chamar uma API externa ou um banco remoto.
- **Isolar cada app no Linux** (network namespace, cgroups ou eBPF). Dá números exatos,
  incluindo o tráfego de saída, mas é bem mais trabalho, só funciona no Linux e, no fundo,
  reconstrói parte do Docker.

**A decidir:**

- se os números do Caddy entram na mesma aba Monitoring ou numa aba própria, junto dos
  domínios;
- quanto tempo de histórico guardar, já que hoje tudo fica só 10 minutos em memória.

## Caddy

### O oxploy cuidar do Caddy sozinho

**Problema.** Hoje o Caddy roda por fora: quem instala o oxploy também instala o Caddy e
deixa ele rodando como serviço da máquina. Para quem quer um binário que faz tudo, é um
passo a mais.

**Objetivo.** Quem instala o oxploy não instalar mais nada.

**Caminho já discutido:** na primeira vez, o oxploy baixa uma versão fixa do Caddy para
`data/caddy`, conferindo o checksum, e inicia ele no grupo de processos dele, como já faz
com os apps: o Caddy sobrevive ao painel reiniciar, e o painel retoma ele quando volta. A
configuração continua indo pela API de admin. Uma variável como `OXPLOY_CADDY` aponta
para um Caddy já instalado, para servidor sem internet ou para quem preferir.

Descartado: rodar o Caddy no Docker. Troca a instalação do Caddy pela do Docker, que é
maior, e um container não alcança os apps em `127.0.0.1` sem rede `host`, que não funciona
igual no Mac.

**A decidir:**

- o que fazer se o Caddy cair enquanto o painel está fora; com o oxploy rodando pelo
  systemd, ele volta em segundos;
- como o Caddy ganha as portas 80 e 443 sem rodar como root, por exemplo com
  `AmbientCapabilities` no serviço do systemd do oxploy.

## Fuso horário

### Passar o fuso das Settings aos apps

**Problema.** O fuso que o super admin escolhe em Settings vale só para o painel: o relógio,
os gráficos, as datas e as marcas que o oxploy escreve nos logs. Os apps continuam no fuso
do sistema, que numa VPS costuma ser UTC.

**Quando.** Junto com as rotinas, as tarefas agendadas dos apps, que precisam de um fuso
para dizer a que horas rodam.

**Caminho.** O oxploy passa `TZ` com o fuso escolhido aos apps, aos builds e aos scripts,
como já passa `PORT`.

**A decidir:**

- se um app que define `TZ` nas variáveis dele ganha do fuso do painel;
- se mudar o fuso reinicia os apps, ou se vale só a partir do próximo start.

## Preview deployments

### Uma cópia do app para cada pull request

**Problema.** A aba Preview deployments de cada serviço existe, mas é só um placeholder.
Para ver uma mudança rodando, hoje é preciso fazer o merge na branch que o serviço
deploya.

**Objetivo.** Cada pull request aberto ganha uma cópia do app no seu próprio subdomínio. O
oxploy refaz o deploy a cada push e apaga a cópia quando o PR fecha.

**Caminho já discutido:**

- **Por PR, não por branch.** Um PR tem início e fim, então o preview é apagado quando ele
  fecha ou é mergeado. Também é a hora em que alguém vai revisar. Por branch, toda `wip`
  ganharia um build e ficaria rodando esquecida.
- **Polling, sem webhook.** A cada 60s o oxploy lista os PRs abertos, como já faz com as
  releases. Assim o painel não precisa estar acessível pelo GitHub.
- **O preview é um serviço filho.** Ele reaproveita o deploy, o runtime, o Caddy, os logs e
  o storage, sem um sistema paralelo. Tem o app name `<app>-pr-<n>` e uma porta própria.
- **Build incremental.** O preview usa o mesmo `CARGO_TARGET_DIR` do serviço principal, o
  que é seguro porque os builds rodam um de cada vez. O deploy de produção sempre passa na
  frente dos previews na fila.
- **Domínio gerado no sslip.io.** Algo como `<app>-pr-<n>.1-2-3-4.sslip.io`. Um domínio
  próprio com wildcard fica para depois.
- **Variáveis herdadas.** O preview copia as variáveis do serviço principal, com uma caixa
  para sobrescrever algumas só nos previews. O app recebe também `OXPLOY_PREVIEW=1` e a URL
  do preview.
- **Storage próprio e vazio.** O banco SQLite nasce zerado, as migrations rodam, e tudo é
  apagado junto com o preview. Copiar o banco de produção colocaria dados reais numa URL
  pública.

**Riscos que definem o desenho:**

- **O código do PR roda no servidor.** O `build.rs` e as macros já executam código no
  build, e o app roda como o usuário `oxploy`. Esse usuário lê o storage dos outros apps e o
  banco do painel, que guarda a chave privada da GitHub App. Por isso só sobem PRs de
  branches do próprio repositório, nunca de fork. Isso importa porque o oxploy é open
  source: num repositório público, qualquer um abre PR de fork.
- **Memória.** Cada preview é mais um processo, e cada push é mais um build. Por isso a
  função começa desligada, com um toggle por serviço, e tem um limite de 3 previews ao
  mesmo tempo. Os PRs além disso esperam como "waiting".

**Fica de fora:**

- **Serviços em modo Release.** Um PR não tem release para baixar. Uma versão futura
  poderia baixar o artifact que o GitHub Actions gera no PR, sem compilar na VPS.

**Precisa mudar:**

- **A GitHub App.** Hoje ela pede só `contents: read` e `metadata: read`. Para listar os
  PRs, precisa de `pull_requests: read`. As apps que já existem precisam aceitar a
  permissão nova no GitHub, uma vez.

**A decidir:**

- os pontos acima, que são recomendações e ainda não foram confirmados;
- se o link do preview aparece no próprio PR, com um comentário ou o botão "View
  deployment" do GitHub. As duas opções precisam de permissão de escrita, então a primeira
  versão mostra o link só no painel.
