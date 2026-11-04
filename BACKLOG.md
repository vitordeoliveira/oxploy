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
