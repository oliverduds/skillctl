mod cli;
mod config;
mod discover;
mod installer;
mod models;
mod scanner;
mod sources;
mod tui;

use anyhow::{bail, Result};
use clap::Parser;
use cli::{Cli, Commands};
use config::{shorten_home, AppConfig};
use discover::{DiscoverClient, TrendingPeriod, TrendingSkill};
use installer::{install_skill, sync_all_symlinks, uninstall_skill, update_skills};
use models::{AgentTarget, SkillRecord, UpdateStatus};
use scanner::scan_all_skills;
use sources::UpdateChecker;
use std::collections::HashSet;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = AppConfig::discover()?;

    match cli.command {
        None | Some(Commands::Tui) => {
            tui::run_tui(config).await?;
        }

        Some(Commands::Scan { all, json }) => {
            let skills = scan_all_skills(&config)?;
            let visible: Vec<&SkillRecord> = skills
                .iter()
                .filter(|s| all || !s.source.is_system_or_cloud())
                .collect();

            if json {
                println!("{}", serde_json::to_string_pretty(&visible)?);
            } else {
                print_cli_table(&config, &visible, false);
                if !all {
                    let hidden = skills.len().saturating_sub(visible.len());
                    if hidden > 0 {
                        println!(
                            "\n\x1b[90m(+ {hidden} skills de sistema/cloud omitidas; use `--all` para exibir todas)\x1b[0m"
                        );
                    }
                }
            }
        }

        Some(Commands::Check {
            all,
            outdated,
            json,
        }) => {
            if !json {
                eprintln!("Varrendo skills e consultando fontes remotas...");
            }
            let mut skills = scan_all_skills(&config)?;
            let checker = UpdateChecker::new();
            checker.check_all(&mut skills).await;

            let visible: Vec<&SkillRecord> = skills
                .iter()
                .filter(|s| {
                    if outdated {
                        s.status.is_outdated()
                    } else {
                        all || !s.source.is_system_or_cloud()
                    }
                })
                .collect();

            if json {
                println!("{}", serde_json::to_string_pretty(&visible)?);
            } else {
                print_cli_table(&config, &visible, true);
                print_check_summary(&skills, all, outdated);
            }
        }

        Some(Commands::Install {
            source,
            skill,
            agents,
            dry_run,
        }) => {
            let targets = parse_agent_targets(agents)?;
            let report = install_skill(&config, &source, skill.as_deref(), &targets, dry_run)?;
            for msg in report.messages {
                println!("{msg}");
            }
        }

        Some(Commands::Update { name, all, dry_run }) => {
            if name.is_none() && !all {
                bail!("Informe o nome da skill (`skillctl update <nome>`) ou use `--all` para atualizar todas as desatualizadas.");
            }
            eprintln!("Verificando skills instaladas...");
            let mut skills = scan_all_skills(&config)?;
            let checker = UpdateChecker::new();
            checker.check_all(&mut skills).await;

            let to_update: Vec<SkillRecord> = if let Some(target_name) = name {
                let matched: Vec<SkillRecord> = skills
                    .into_iter()
                    .filter(|s| s.name.eq_ignore_ascii_case(&target_name))
                    .collect();
                if matched.is_empty() {
                    bail!("Skill '{}' não encontrada neste computador.", target_name);
                }
                matched
            } else {
                skills
                    .into_iter()
                    .filter(|s| s.status.is_outdated() && s.source.can_update())
                    .collect()
            };

            if to_update.is_empty() {
                println!("\x1b[32m✓ Todas as skills verificáveis já estão atualizadas!\x1b[0m");
                return Ok(());
            }

            let report = update_skills(&config, &to_update, dry_run)?;
            for msg in report.messages {
                println!("{msg}");
            }
        }

        Some(Commands::Uninstall { name, dry_run }) => {
            let report = uninstall_skill(&config, &name, dry_run)?;
            for msg in report.messages {
                println!("{msg}");
            }
        }

        Some(Commands::Link { agents, dry_run }) => {
            let targets = parse_agent_targets(agents)?;
            let report = sync_all_symlinks(&config, &targets, dry_run)?;
            if report.messages.is_empty() {
                println!("\x1b[32m✓ Todos os symlinks já estão sincronizados.\x1b[0m");
            } else {
                for msg in report.messages {
                    println!("{msg}");
                }
            }
        }

        Some(Commands::Top {
            period,
            limit,
            json,
        }) => {
            let Some(p) = TrendingPeriod::from_cli_str(&period) else {
                bail!("Período inválido '{}'. Use: dia, semana, mes, alta ou total.", period);
            };
            if !json {
                eprintln!("Consultando ranking '{}' em skills.sh...", p.label());
            }
            let client = DiscoverClient::new();
            let items = client.fetch_trending(p, limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                let installed: HashSet<String> = scan_all_skills(&config)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.name.to_lowercase())
                    .collect();
                print_trending_table(p.label(), &items, &installed);
            }
        }

        Some(Commands::Search { query, limit, json }) => {
            if !json {
                eprintln!("Buscando '{query}' em skills.sh...");
            }
            let client = DiscoverClient::new();
            let items = client.search_skills(&query, limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                let installed: HashSet<String> = scan_all_skills(&config)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.name.to_lowercase())
                    .collect();
                print_trending_table(&format!("Busca: \"{query}\""), &items, &installed);
            }
        }
    }

    Ok(())
}

fn parse_agent_targets(agents: Option<Vec<String>>) -> Result<Vec<AgentTarget>> {
    let Some(list) = agents else {
        return Ok(AgentTarget::default_symlink_targets());
    };
    let mut parsed = Vec::new();
    for item in list {
        if item.eq_ignore_ascii_case("all") {
            return Ok(AgentTarget::default_symlink_targets());
        }
        match AgentTarget::from_cli_str(&item) {
            Some(t) => parsed.push(t),
            None => bail!(
                "Agente desconhecido '{}'. Use: claude, antigravity, codex ou all.",
                item
            ),
        }
    }
    Ok(parsed)
}

fn print_cli_table(config: &AppConfig, skills: &[&SkillRecord], show_status: bool) {
    if skills.is_empty() {
        println!("Nenhuma skill encontrada para este filtro.");
        return;
    }

    if show_status {
        println!(
            "\x1b[1m{:<10} {:<30} {:<6} {:<30} {}\x1b[0m",
            "STATUS", "SKILL", "AGTS", "ORIGEM", "DETALHE"
        );
        println!("{}", "─".repeat(108));
    } else {
        println!(
            "\x1b[1m{:<30} {:<6} {:<30} {}\x1b[0m",
            "SKILL", "AGTS", "ORIGEM", "CAMINHO"
        );
        println!("{}", "─".repeat(104));
    }

    for s in skills {
        let agents = s.agents_badge();
        let src = truncate(&s.source.short_label(), 28);
        let name = truncate(&s.name, 28);

        if show_status {
            let (color, badge, detail) = match &s.status {
                UpdateStatus::UpdateAvailable {
                    current,
                    latest,
                    summary,
                    ..
                } => (
                    "\x1b[1;33m",
                    "↑ UPDATE",
                    format!("{current} → {latest} ({summary})"),
                ),
                UpdateStatus::UpToDate { detail } => ("\x1b[32m", "✓ OK", detail.clone()),
                UpdateStatus::NotTrackable { reason } => ("\x1b[90m", "• LOCAL", reason.clone()),
                UpdateStatus::Error { message } => ("\x1b[31m", "! ERRO", message.clone()),
                UpdateStatus::Unchecked => ("\x1b[90m", "· PEND", String::new()),
            };
            println!(
                "{color}{:<10}\x1b[0m {:<30} \x1b[34m{:<6}\x1b[0m {:<30} {}",
                badge, name, agents, src, detail
            );
        } else {
            let path_str = shorten_home(&s.canonical_path, &config.home_dir);
            println!(
                "{:<30} \x1b[34m{:<6}\x1b[0m {:<30} {}",
                name, agents, src, path_str
            );
        }
    }

    println!("\n\x1b[90mLegenda AGTS: A=~/.agents/skills  C=Claude  G=Antigravity  X=Codex\x1b[0m");
}

fn print_check_summary(skills: &[SkillRecord], showed_all: bool, only_outdated: bool) {
    let outdated: Vec<&SkillRecord> = skills.iter().filter(|s| s.status.is_outdated()).collect();
    let up_to_date = skills.iter().filter(|s| s.status.is_up_to_date()).count();
    let errors = skills
        .iter()
        .filter(|s| matches!(s.status, UpdateStatus::Error { .. }))
        .count();

    println!(
        "\nResumo: \x1b[1;33m{} para atualizar\x1b[0m | \x1b[32m{} em dia\x1b[0m | {} erro(s) | {} total",
        outdated.len(),
        up_to_date,
        errors,
        skills.len()
    );

    if !outdated.is_empty() {
        println!("\nPara atualizar tudo de uma vez:");
        println!("  \x1b[36mskillctl update --all\x1b[0m  (ou teste antes com `--dry-run`)");
    }

    if !showed_all && !only_outdated {
        let hidden = skills
            .iter()
            .filter(|s| s.source.is_system_or_cloud())
            .count();
        if hidden > 0 {
            println!(
                "\x1b[90m(+ {hidden} skills embutidas/cloud omitidas; use `--all` para ver todas)\x1b[0m"
            );
        }
    }
}

fn print_trending_table(title: &str, items: &[TrendingSkill], installed: &HashSet<String>) {
    if items.is_empty() {
        println!("Nenhuma skill encontrada.");
        return;
    }

    println!("\x1b[1;36mRanking skills.sh — {title}\x1b[0m\n");
    println!(
        "\x1b[1m{:<4} {:<30} {:<18} {:<10} {:<30} {}\x1b[0m",
        "#", "SKILL", "DOWNLOADS", "TEND.", "REPOSITÓRIO", "STATUS"
    );
    println!("{}", "─".repeat(108));

    for it in items {
        let name = truncate(&it.name, 28);
        let src = truncate(&it.source, 28);
        let spark = it.trend_sparkline.as_deref().unwrap_or("—");
        let is_inst = installed.contains(&it.name.to_lowercase())
            || installed.contains(&it.skill_id.to_lowercase());
        let status = if is_inst {
            "\x1b[32m✓ instalada\x1b[0m"
        } else if it.is_official {
            "\x1b[36m★ oficial\x1b[0m"
        } else {
            "\x1b[90mdisponível\x1b[0m"
        };
        println!(
            "{:<4} {:<30} \x1b[33m{:<18}\x1b[0m \x1b[36m{:<10}\x1b[0m {:<30} {}",
            format!("#{}", it.rank),
            name,
            it.metric_label,
            spark,
            src,
            status
        );
    }

    println!("\nPara instalar qualquer uma delas:");
    println!("  \x1b[36mskillctl install <repositorio>@<skill>\x1b[0m  (ou abra `skillctl` e use a opção [3])");
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
