use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "skillctl",
    version,
    about = "Gerenciador unificado de Agent Skills (CLI + TUI) em Rust",
    long_about = "Descobre todas as skills instaladas no computador, verifica atualizações em todas as fontes (GitHub, npx skills, clones locais, binários gerenciados), instala novas skills em ~/.agents/skills com symlinks automáticos e atualiza com segurança."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Abre o painel interativo no terminal (TUI com Ratatui)
    Tui,

    /// Faz a varredura local de todas as skills instaladas no computador (offline)
    Scan {
        /// Inclui skills embutidas (Antigravity/Codex) e sincronizadas (Claude.ai)
        #[arg(short, long)]
        all: bool,

        /// Emite resultado em JSON
        #[arg(long)]
        json: bool,
    },

    /// Varre o PC e verifica atualizações em todas as fontes remotas
    Check {
        /// Inclui skills embutidas e sincronizadas na listagem
        #[arg(short, long)]
        all: bool,

        /// Mostra apenas as skills que precisam de atualização
        #[arg(long)]
        outdated: bool,

        /// Emite resultado em JSON
        #[arg(long)]
        json: bool,
    },

    /// Instala uma ou mais skills a partir do GitHub (owner/repo, owner/repo@skill, URL) ou pasta local
    Install {
        /// Origem da skill: `owner/repo`, `owner/repo@skill`, URL Git ou caminho local
        source: String,

        /// Nome específico da skill quando o repositório contém várias
        #[arg(short, long)]
        skill: Option<String>,

        /// Agentes onde criar symlinks (padrão: claude,antigravity,codex)
        #[arg(short, long, value_delimiter = ',')]
        agents: Option<Vec<String>>,

        /// Simula a instalação sem modificar arquivos
        #[arg(long)]
        dry_run: bool,
    },

    /// Atualiza uma skill específica ou todas as desatualizadas
    Update {
        /// Nome da skill para atualizar (omitir junto com --all atualiza todas)
        name: Option<String>,

        /// Atualiza todas as skills com atualização disponível
        #[arg(short, long)]
        all: bool,

        /// Simula a atualização sem executar mudanças
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove uma skill instalada em ~/.agents/skills e seus symlinks nos agentes
    Uninstall {
        /// Nome da skill a remover
        name: String,

        /// Simula a remoção sem apagar arquivos
        #[arg(long)]
        dry_run: bool,
    },

    /// Sincroniza os symlinks de ~/.agents/skills para Claude, Antigravity e Codex
    Link {
        /// Agentes alvo (padrão: claude,antigravity,codex)
        #[arg(short, long, value_delimiter = ',')]
        agents: Option<Vec<String>>,

        /// Simula a criação de symlinks sem modificar o disco
        #[arg(long)]
        dry_run: bool,
    },
}
