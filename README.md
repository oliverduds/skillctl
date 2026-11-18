# skillctl

Gerenciador unificado de **Agent Skills** em Rust para **macOS, Linux e Windows**, com **menu interativo numerado** no terminal.

- **Varredura completa:** descobre e deduplica skills em `~/.agents/skills`, `~/.claude/skills` (incluindo `synced/`), `~/.gemini/config/skills`, `~/.gemini/antigravity/builtin/skills`, `~/.codex/skills` (e `.system/`), clones locais (ex.: `~/unslop`) e projetos locais.
- **Verificação de atualizações:** consulta em paralelo **GitHub**, **`npx skills`** (`~/.agents/.skill-lock.json`), clones Git locais, skills third-party versionadas (SHA-1 de blob Git) e binários gerenciados (ex.: `ai-memory`).
- **Descoberta de skills mais baixadas (v0.2):** ranking ao vivo do `skills.sh` por **Dia (24h)**, **Semana (7 dias)**, **Mês (30 dias)**, **Em alta hoje**, **Todos os tempos** e **busca por palavra-chave**, com instalação direta em 1 tecla.
- **Instalação e symlinks automáticos:** instala em `~/.agents/skills/<nome>` e cria symlinks seguros para **Claude**, **Antigravity** e **Codex** (com fallback automático para NTFS Junction no Windows, sem exigir privilégio de administrador).

---

## Instalação rápida

### macOS e Linux

```sh
curl -fsSL https://raw.githubusercontent.com/oliverduds/skillctl/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/oliverduds/skillctl/main/install.ps1 | iex
```

### Via Cargo (qualquer sistema com Rust)

```sh
cargo install --git https://github.com/oliverduds/skillctl
```

---

## Como usar

Depois de instalar, basta abrir o terminal e digitar:

```sh
skillctl
```

Você navega usando **opções numeradas** (ou `↑`/`↓` + `Enter`):

| Opção | O que faz |
| --- | --- |
| **`[1]`** | **Verificar atualizações no PC** — varre todas as skills e compara com as origens remotas |
| **`[2]`** | **Atualizar skills desatualizadas** — atualiza todas de uma vez (`[1]`) ou uma específica (`[2]`..`[9]`) |
| **`[3]`** | **Descobrir skills mais baixadas (Dia / Semana / Mês)** — ranking ao vivo do `skills.sh` + busca + instalação direta |
| **`[4]`** | **Instalar nova skill (GitHub / URL / Local)** — aceita `owner/repo`, `owner/repo@skill`, URL do GitHub ou pasta local |
| **`[5]`** | **Ver todas as skills instaladas** — lista com status, agentes vinculados e tela de detalhes |
| **`[6]`** | **Sincronizar symlinks entre agentes** — exibe prévia antes de criar os links para Claude, Antigravity e Codex |
| **`[7]`** | **Remover uma skill instalada** — escolhe pelo número com confirmação |
| **`[0]`** | **Voltar / Sair** |

*(Subcomandos opcionais de CLI também funcionam para automação/scripts: `skillctl scan`, `skillctl check`, `skillctl top dia|semana|mes|alta|total`, `skillctl search <termo>`, `skillctl install <origem>`, `skillctl update --all`, `skillctl link`, `skillctl uninstall <nome>`.)*

## Licença

MIT — veja [LICENSE](./LICENSE).
