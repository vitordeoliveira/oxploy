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
