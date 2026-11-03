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
