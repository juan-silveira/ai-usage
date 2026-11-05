# AI Usage

[![CI](https://github.com/juan-silveira/ai-usage/actions/workflows/ci.yml/badge.svg)](https://github.com/juan-silveira/ai-usage/actions/workflows/ci.yml)

Monitor de uso das assinaturas de IA (Claude Code, OpenAI Codex, …) para
**Linux, Windows e macOS**. Fica na bandeja do sistema e mostra:

- **Limites** (a cada minuto): o mesmo que o `/usage` do Claude Code — janela
  de 5 h, limite semanal, limites por modelo e quando cada um reinicia.
- **Sessões ao vivo** (via hooks): cada sessão do Claude Code / Codex aberta,
  se está trabalhando, concluída ou **esperando você** — com notificação na
  hora e lembrete a cada 2 min até você responder.

Não pede senha nem login próprio: reaproveita as credenciais que as CLIs
já guardam na máquina, **somente leitura** (nunca renova nem reescreve
tokens, para não deslogar a CLI).

## Instalação

Baixe o instalador do seu sistema na página de
[Releases](https://github.com/juan-silveira/ai-usage/releases):
`.deb`/`.rpm`/`.AppImage` (Linux), `.msi`/`-setup.exe` (Windows) ou `.dmg`
(macOS — `aarch64` para Apple Silicon, `x64` para Intel). Os builds não são
assinados: no macOS abra com clique direito → Abrir; no Windows, "Mais
informações" → "Executar assim mesmo".

## Estrutura

```
crates/core      biblioteca Rust: provedores, modelo de dados, HTTP
crates/cli       `ai-usage` — imprime uso no terminal ou em JSON
app/src-tauri    app desktop (Tauri 2): bandeja + janela flutuante
app/ui           frontend estático (HTML/CSS/JS, sem build)
```

## Provedores

| Provedor | Credencial lida | Endpoint |
|---|---|---|
| Claude | `~/.claude/.credentials.json` (ou `$CLAUDE_CONFIG_DIR`); no macOS, Keychain `Claude Code-credentials` | `GET api.anthropic.com/api/oauth/usage` |
| Codex | `~/.codex/auth.json` (ou `$CODEX_HOME`) | `GET chatgpt.com/backend-api/wham/usage` |

Ambos os endpoints são internos/não documentados e podem mudar.
Para adicionar um provedor, implemente o trait `Provider` em
`crates/core/src/providers/` e registre-o em `all_providers()`.

## Conexões

Na aba ⚙ → Conexões, cada provedor mostra o status (conectado + plano, login
expirado, não conectado) e um botão **Conectar/Reconectar** que roda o login
oficial da CLI (`claude auth login`, `codex login`) — ele abre o navegador; se
não abrir, o app mostra o link, e há um campo para colar o código caso a CLI
peça. Sem a CLI instalada, mostra o comando de instalação. O app encontra as
CLIs mesmo fora do PATH do desktop (asdf, nvm, volta, npm global).

O interruptor **Sessões ao vivo** instala/remove os hooks. Eles chamam o
próprio executável do app (`ai-usage-app hook claude`), então não é preciso
instalar a CLI `ai-usage` separadamente.

O app inicia com o sistema (ligado por padrão na primeira execução, direto
na barra, sem abrir a janela) e tem tema claro/escuro/seguir o sistema para a
janela e o widget.

## Sessões e alertas

O comando `ai-usage hook <claude|codex>` é registrado como hook dos agentes.
A cada evento (prompt enviado, ferramenta usada, pedido de permissão, turno
concluído, sessão encerrada) ele grava um JSON pequeno por sessão em
`~/.local/state/ai-usage/sessions/` (Linux), `~/Library/Application Support/ai-usage/sessions/`
(macOS) ou `%LOCALAPPDATA%\ai-usage\sessions\` (Windows). O app lê essa pasta
a cada 2 s; o hook nunca falha nem imprime nada, então não atrapalha o agente
mesmo com o app fechado.

| Evento do agente | Estado |
|---|---|
| `SessionStart` | ociosa |
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | trabalhando |
| `Notification` (permissão/pergunta), `PermissionRequest` | **precisa de você** → alerta + lembrete a cada 2 min (máx. 5) |
| `Stop` | concluída → alerta (🎉 se o turno durou ≥ 5 min) |
| `SessionEnd` | removida |

Clicar numa sessão "precisa de você" na janela silencia os lembretes.

```sh
cargo install --path crates/cli   # instala o binário em ~/.cargo/bin/ai-usage
ai-usage hooks install            # Claude (~/.claude/settings.json) e Codex (~/.codex/hooks.json)
ai-usage hooks install claude     # só um provedor
ai-usage hooks status
ai-usage sessions                 # lista as sessões no terminal
ai-usage hooks uninstall          # remove só as entradas do ai-usage
```

A instalação mescla com o que já existe (outros hooks e configurações ficam
intactos), é idempotente e guarda um backup `*.ai-usage.bak` do arquivo original.

## Uso

### CLI

```sh
cargo run -p ai-usage-cli              # tabela
cargo run -p ai-usage-cli -- --json    # JSON (scripts, barras de status, displays)
cargo run -p ai-usage-cli -- --watch 60
```

### App desktop

Pré-requisitos: Rust estável recente ([rustup](https://rustup.rs)) e as
[dependências do Tauri](https://v2.tauri.app/start/prerequisites/) para o seu SO.
No Ubuntu/Debian:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev
```

```sh
cargo run -p ai-usage-app                 # desenvolvimento
cargo install tauri-cli --version '^2'    # uma vez
cd app/src-tauri && cargo tauri build     # instaladores (.deb/.AppImage, .msi, .dmg)
```

Na barra do sistema aparece um item por provedor, como o applet de clima:
logo + uso (ex.: `✳ 7/17%`). Nas preferências da janela dá para escolher o
número (maior uso, sessão 5h, sessão/semanal ou só o ícone do app) e, por
provedor, mostrar/ocultar o item, o logo e um nome personalizável.
Quando uma sessão espera você, o item ganha `⚠1`.

Clique esquerdo num item mostra/oculta a janela; o botão direito abre o
menu (atualizar agora, sair). Fechar a janela mantém o app na bandeja.

No Cinnamon (Linux Mint) o app usa o ícone nativo XApp (via libxapp), pois o
`xapp-sn-watcher` descarta o texto de ícones AppIndicator. No macOS o texto
aparece ao lado do ícone; no Windows a bandeja só exibe ícone e tooltip.

Faixas de alerta de uso, separadas para a sessão (5 h) e a semanal (ex.: sessão
`80, 95`, semanal `90`): cada faixa notifica uma vez por janela, com som próprio,
e define as cores: verde abaixo da primeira faixa, amarelo a partir dela e
vermelho só em 100%.

Widget flutuante (opcional): janela transparente, sempre visível, que não
recebe cliques (eles passam para o que está atrás). Para reposicionar, use
"Mover" nas preferências, arraste e dê clique duplo para fixar. A cor e a
opacidade do fundo são personalizáveis (o texto se ajusta ao brilho da cor), e
as sessões podem aparecer resumidas ("2 trabalhando"), uma por linha
(projeto, estado e tempo, as mais urgentes primeiro) ou ocultas.

O app consulta as releases do GitHub ao iniciar e a cada 12 h e avisa quando há
versão nova (banner + notificação com link para baixar).

Os endpoints de uso têm limite de consultas: em HTTP 429 o app respeita o
`Retry-After` (1–30 min, com recuo exponencial) e continua mostrando os últimos
dados, marcados como desatualizados. O estado fica num cache em disco
compartilhado entre app e CLI, então reiniciar o app não gera consultas extras.

Alertas sonoros (gerados pelo app, sem arquivos de áudio): um sino curto e
insistente quando uma sessão precisa de você e um arpejo quando conclui.
Cada um pode ser desligado e ouvido nas preferências.

## CI e releases

- `.github/workflows/ci.yml` — a cada push/PR: `cargo fmt --check`, `clippy -D
  warnings` e testes no Linux; build + testes no Windows e no macOS.
- `.github/workflows/release.yml` — ao enviar uma tag `vX.Y.Z` (igual à
  `version` do `Cargo.toml` do workspace), gera os instaladores dos três
  sistemas e cria uma release **rascunho** com eles. Revise e clique em
  "Publish": o app só enxerga releases publicadas.

```sh
# depois de atualizar version em Cargo.toml
git tag v0.2.0 && git push origin v0.2.0
```

## Roadmap

- [x] Estado das sessões ativas via hooks, com alertas e lembretes
- [ ] Modo foco / reunião (silenciar alertas), som configurável
- [ ] Clicar na notificação foca o terminal da sessão
- [ ] API local (HTTP/WebSocket) para widgets e displays externos
- [ ] Mais provedores: Gemini CLI, Cursor, Copilot, OpenRouter
- [x] Uso na barra do sistema com logo do provedor e nome personalizável
- [x] Alertas sonoros
- [x] Faixas de alerta de uso por janela
- [x] Widget flutuante transparente e com clique atravessando
- [x] Aviso de nova versão (GitHub Releases)
- [ ] Instalação automática de atualizações (tauri-plugin-updater + releases assinadas no CI)
- [x] Iniciar com o sistema
- [x] Botão de conectar (login pela CLI oficial) e hooks pelo app
- [x] Tema claro/escuro
- [x] CI com builds para os três SOs
- [ ] Assinatura de código (macOS notarização, Windows Authenticode)

## Referências

Projetos que inspiraram a descoberta dos endpoints:
[CodexBar](https://github.com/steipete/CodexBar),
[Win-CodexBar](https://github.com/nesszer/Win-CodexBar),
[Claude-Code-Usage-Monitor](https://github.com/CodeZeno/Claude-Code-Usage-Monitor).

## Créditos

Logos do Claude e da OpenAI: [Simple Icons](https://simpleicons.org) (CC0).
As marcas pertencem aos respectivos donos.

## Licença

MIT
