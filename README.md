# IA Usage

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

## Estrutura

```
crates/core      biblioteca Rust: provedores, modelo de dados, HTTP
crates/cli       `ia-usage` — imprime uso no terminal ou em JSON
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

## Sessões e alertas

O comando `ia-usage hook <claude|codex>` é registrado como hook dos agentes.
A cada evento (prompt enviado, ferramenta usada, pedido de permissão, turno
concluído, sessão encerrada) ele grava um JSON pequeno por sessão em
`~/.local/state/ia-usage/sessions/` (Linux), `~/Library/Application Support/ia-usage/sessions/`
(macOS) ou `%LOCALAPPDATA%\ia-usage\sessions\` (Windows). O app lê essa pasta
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
cargo install --path crates/cli   # instala o binário em ~/.cargo/bin/ia-usage
ia-usage hooks install            # Claude (~/.claude/settings.json) e Codex (~/.codex/hooks.json)
ia-usage hooks install claude     # só um provedor
ia-usage hooks status
ia-usage sessions                 # lista as sessões no terminal
ia-usage hooks uninstall          # remove só as entradas do ia-usage
```

A instalação mescla com o que já existe (outros hooks e configurações ficam
intactos), é idempotente e guarda um backup `*.ia-usage.bak` do arquivo original.

## Uso

### CLI

```sh
cargo run -p ia-usage-cli              # tabela
cargo run -p ia-usage-cli -- --json    # JSON (scripts, barras de status, displays)
cargo run -p ia-usage-cli -- --watch 60
```

### App desktop

Pré-requisitos: Rust estável recente ([rustup](https://rustup.rs)) e as
[dependências do Tauri](https://v2.tauri.app/start/prerequisites/) para o seu SO.
No Ubuntu/Debian:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev
```

```sh
cargo run -p ia-usage-app                 # desenvolvimento
cargo install tauri-cli --version '^2'    # uma vez
cd app/src-tauri && cargo tauri build     # instaladores (.deb/.AppImage, .msi, .dmg)
```

Clique esquerdo no ícone da bandeja mostra/oculta a janela; o botão direito
abre o menu (atualizar agora, sair). Fechar a janela mantém o app na bandeja.

## Roadmap

- [x] Estado das sessões ativas via hooks, com alertas e lembretes
- [ ] Modo foco / reunião (silenciar alertas), som configurável
- [ ] Clicar na notificação foca o terminal da sessão
- [ ] API local (HTTP/WebSocket) para widgets e displays externos
- [ ] Mais provedores: Gemini CLI, Cursor, Copilot, OpenRouter
- [ ] Ícone da bandeja com a porcentagem desenhada
- [ ] Notificação ao passar de 80 % / 95 %
- [ ] Iniciar com o sistema
- [ ] CI com builds para os três SOs

## Referências

Projetos que inspiraram a descoberta dos endpoints:
[CodexBar](https://github.com/steipete/CodexBar),
[Win-CodexBar](https://github.com/nesszer/Win-CodexBar),
[Claude-Code-Usage-Monitor](https://github.com/CodeZeno/Claude-Code-Usage-Monitor).

## Licença

MIT
