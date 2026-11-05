# IA Usage

Monitor de uso das assinaturas de IA (Claude Code, OpenAI Codex, …) para
**Linux, Windows e macOS**. Fica na bandeja do sistema, atualiza a cada
minuto e mostra o mesmo que o `/usage` do Claude Code: janela de 5 h,
limite semanal, limites por modelo e quando cada um reinicia.

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

- [ ] Estado das sessões ativas (trabalhando / esperando você / concluída) via hooks do Claude Code e eventos do Codex, com alertas
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
