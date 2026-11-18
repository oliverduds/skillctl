use crate::config::{parse_frontmatter, shorten_home, AppConfig};
use crate::models::{extract_github_slug, AgentTarget, RegistryEntry, SkillRecord, SkillSource};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use walkdir::WalkDir;

#[cfg(windows)]
const NPX_CMD: &str = "npx.cmd";
#[cfg(not(windows))]
const NPX_CMD: &str = "npx";

/// Creates a directory symlink (or NTFS junction fallback on Windows when Developer Mode is off).
fn create_dir_link(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
            .with_context(|| format!("Falha ao criar symlink {}", dst.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(src, dst).is_ok() {
            return Ok(());
        }
        // Fallback to NTFS directory junction (`mklink /J`), which requires no admin rights
        let out = Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &dst.to_string_lossy(),
                &src.to_string_lossy(),
            ])
            .output()
            .context("Falha ao executar mklink /J no Windows")?;
        if !out.status.success() {
            bail!(
                "Não foi possível criar link/junction {} -> {}: {}",
                dst.display(),
                src.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

fn remove_dir_link(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::remove_file(path)
            .with_context(|| format!("Falha ao remover symlink {}", path.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        fs::remove_dir(path)
            .or_else(|_| fs::remove_file(path))
            .with_context(|| format!("Falha ao remover link {}", path.display()))?;
        Ok(())
    }
}

fn is_link_metadata(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        // NTFS directory junctions have FILE_ATTRIBUTE_REPARSE_POINT (0x0400)
        use std::os::windows::fs::MetadataExt;
        if (meta.file_attributes() & 0x0400) != 0 {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct ActionReport {
    pub messages: Vec<String>,
}

impl ActionReport {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn push(&mut self, msg: impl Into<String>) {
        self.messages.push(msg.into());
    }
}

/// Parses specs like:
/// - `owner/repo`
/// - `owner/repo@skill-name`
/// - `https://github.com/owner/repo`
/// - `/path/to/local/skill`
#[derive(Debug, Clone)]
pub enum InstallSource {
    GitRepo {
        url: String,
        slug: String,
        inline_skill: Option<String>,
    },
    LocalDir(PathBuf),
}

pub fn parse_install_source(raw: &str) -> Result<InstallSource> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Origem da skill não pode estar vazia");
    }

    let local_candidate = expand_tilde(trimmed);
    if local_candidate.exists() {
        return Ok(InstallSource::LocalDir(fs::canonicalize(local_candidate)?));
    }

    // Handle `owner/repo@skill`
    if !trimmed.contains("://") && !trimmed.starts_with("git@") {
        let (repo_part, inline_skill) = match trimmed.split_once('@') {
            Some((r, s)) => (r.trim(), Some(s.trim().to_string())),
            None => (trimmed, None),
        };
        let parts: Vec<&str> = repo_part.split('/').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            let slug = format!("{}/{}", parts[0], parts[1].trim_end_matches(".git"));
            let url = format!("https://github.com/{slug}.git");
            return Ok(InstallSource::GitRepo {
                url,
                slug,
                inline_skill,
            });
        }
        bail!(
            "Origem inválida '{}'. Use `owner/repo`, `owner/repo@skill`, URL Git ou pasta local.",
            trimmed
        );
    }

    let slug = extract_github_slug(trimmed).unwrap_or_else(|| trimmed.to_string());
    let url = if trimmed.ends_with(".git") {
        trimmed.to_string()
    } else if trimmed.starts_with("https://github.com/") {
        format!("{}.git", trimmed.trim_end_matches('/'))
    } else {
        trimmed.to_string()
    };

    Ok(InstallSource::GitRepo {
        url,
        slug,
        inline_skill: None,
    })
}

pub fn install_skill(
    config: &AppConfig,
    source_spec: &str,
    explicit_skill: Option<&str>,
    targets: &[AgentTarget],
    dry_run: bool,
) -> Result<ActionReport> {
    let mut report = ActionReport::new();
    let parsed = parse_install_source(source_spec)?;
    let unified_root = config.unified_skills_dir();

    match parsed {
        InstallSource::LocalDir(local_path) => {
            let discovered = discover_skills_in_dir(&local_path);
            if discovered.is_empty() {
                bail!("Nenhum SKILL.md encontrado em {}", local_path.display());
            }
            let selected = filter_discovered(discovered, explicit_skill)?;
            for (skill_name, skill_dir, _subpath) in selected {
                let dest = unified_root.join(&skill_name);
                if dry_run {
                    report.push(format!(
                        "[dry-run] Copiaria '{}' -> {}",
                        skill_name,
                        shorten_home(&dest, &config.home_dir)
                    ));
                } else {
                    copy_dir_clean(&skill_dir, &dest)?;
                    report.push(format!(
                        "✓ Instalada '{}' em {}",
                        skill_name,
                        shorten_home(&dest, &config.home_dir)
                    ));
                    link_single_skill(config, &skill_name, &dest, targets, false, &mut report)?;
                }
            }
        }

        InstallSource::GitRepo {
            url,
            slug,
            inline_skill,
        } => {
            let want_skill = explicit_skill.or(inline_skill.as_deref());
            let tmp = TempDir::new().context("Falha ao criar pasta temporária")?;
            let clone_dir = tmp.path().join("repo");

            let status = Command::new("git")
                .args(["clone", "--depth", "1", &url, &clone_dir.to_string_lossy()])
                .output()
                .context("Falha ao executar git clone")?;

            if !status.status.success() {
                let stderr = String::from_utf8_lossy(&status.stderr);
                bail!("git clone falhou para {}: {}", url, stderr.trim());
            }

            let commit_out = Command::new("git")
                .args(["-C", &clone_dir.to_string_lossy(), "rev-parse", "HEAD"])
                .output()?;
            let commit = String::from_utf8_lossy(&commit_out.stdout).trim().to_string();

            let branch_out = Command::new("git")
                .args([
                    "-C",
                    &clone_dir.to_string_lossy(),
                    "rev-parse",
                    "--abbrev-ref",
                    "HEAD",
                ])
                .output()
                .ok();
            let branch = branch_out
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|b| !b.is_empty() && b != "HEAD");

            let discovered = discover_skills_in_dir(&clone_dir);
            if discovered.is_empty() {
                bail!("Nenhum arquivo SKILL.md encontrado no repositório {slug}");
            }

            let selected = filter_discovered(discovered, want_skill)?;
            let mut registry = config.load_registry();
            let now = Utc::now();

            for (skill_name, skill_dir, subpath) in selected {
                let dest = unified_root.join(&skill_name);
                if dry_run {
                    report.push(format!(
                        "[dry-run] Instalaria '{}' ({} @ {}) em {}",
                        skill_name,
                        slug,
                        &commit[..commit.len().min(7)],
                        shorten_home(&dest, &config.home_dir)
                    ));
                    link_single_skill(config, &skill_name, &dest, targets, true, &mut report)?;
                    continue;
                }

                copy_dir_clean(&skill_dir, &dest)?;
                registry.skills.insert(
                    skill_name.clone(),
                    RegistryEntry {
                        name: skill_name.clone(),
                        repo: slug.clone(),
                        url: url.clone(),
                        subpath: subpath.clone(),
                        commit: commit.clone(),
                        branch: branch.clone(),
                        installed_at: registry
                            .skills
                            .get(&skill_name)
                            .map(|e| e.installed_at)
                            .unwrap_or(now),
                        updated_at: now,
                    },
                );

                report.push(format!(
                    "✓ Instalada '{}' ({} @ {}) -> {}",
                    skill_name,
                    slug,
                    &commit[..commit.len().min(7)],
                    shorten_home(&dest, &config.home_dir)
                ));
                link_single_skill(config, &skill_name, &dest, targets, false, &mut report)?;
            }

            if !dry_run {
                config.save_registry(&registry)?;
            }
        }
    }

    Ok(report)
}

pub fn sync_all_symlinks(
    config: &AppConfig,
    targets: &[AgentTarget],
    dry_run: bool,
) -> Result<ActionReport> {
    let mut report = ActionReport::new();
    let unified_root = config.unified_skills_dir();
    if !unified_root.is_dir() {
        bail!(
            "Diretório unificado {} não encontrado",
            unified_root.display()
        );
    }

    let mut skills = Vec::new();
    for entry in fs::read_dir(&unified_root)?.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("SKILL.md").is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                skills.push((name.to_string(), p));
            }
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, src) in skills {
        link_single_skill(config, &name, &src, targets, dry_run, &mut report)?;
    }

    Ok(report)
}

fn link_single_skill(
    config: &AppConfig,
    skill_name: &str,
    canonical_src: &Path,
    targets: &[AgentTarget],
    dry_run: bool,
    report: &mut ActionReport,
) -> Result<()> {
    for target in targets {
        if *target == AgentTarget::Agents {
            continue;
        }
        let target_root = target.dir(&config.home_dir);
        let link_path = target_root.join(skill_name);

        if let Ok(meta) = fs::symlink_metadata(&link_path) {
            if is_link_metadata(&meta) {
                // Already a symlink/junction: verify if it points to canonical_src
                if let Ok(resolved) = fs::canonicalize(&link_path) {
                    if let Ok(src_canon) = fs::canonicalize(canonical_src) {
                        if resolved == src_canon {
                            continue;
                        }
                    }
                }
                if dry_run {
                    report.push(format!(
                        "  [dry-run] Atualizaria symlink {}",
                        shorten_home(&link_path, &config.home_dir)
                    ));
                } else {
                    remove_dir_link(&link_path)?;
                    create_dir_link(canonical_src, &link_path)?;
                    report.push(format!(
                        "  ↳ Symlink atualizado: {}",
                        shorten_home(&link_path, &config.home_dir)
                    ));
                }
            } else {
                // Real directory — do NOT overwrite!
                report.push(format!(
                    "  ⚠ Pulado {} (já existe pasta real, não symlink)",
                    shorten_home(&link_path, &config.home_dir)
                ));
            }
        } else if dry_run {
            report.push(format!(
                "  [dry-run] Criaria symlink {} -> {}",
                shorten_home(&link_path, &config.home_dir),
                shorten_home(canonical_src, &config.home_dir)
            ));
        } else {
            fs::create_dir_all(&target_root)?;
            create_dir_link(canonical_src, &link_path)?;
            report.push(format!(
                "  ↳ Linkado em {}",
                shorten_home(&link_path, &config.home_dir)
            ));
        }
    }
    Ok(())
}

pub fn update_skills(
    config: &AppConfig,
    skills: &[SkillRecord],
    dry_run: bool,
) -> Result<ActionReport> {
    let mut report = ActionReport::new();
    let mut upgraded_binaries: HashSet<PathBuf> = HashSet::new();
    let mut ran_npx_update = false;
    let mut pulled_repos: HashSet<PathBuf> = HashSet::new();
    let targets = AgentTarget::default_symlink_targets();

    for skill in skills {
        match &skill.source {
            SkillSource::SkillctlRegistry { url, subpath, .. } => {
                if dry_run {
                    report.push(format!(
                        "[dry-run] Atualizaria '{}' via Git ({})",
                        skill.name, url
                    ));
                    continue;
                }
                let sub_report = update_registry_skill(config, skill, url, subpath, &targets)?;
                report.messages.extend(sub_report.messages);
            }

            SkillSource::NpxSkillLock { source, .. } => {
                if ran_npx_update {
                    continue;
                }
                ran_npx_update = true;
                if dry_run {
                    report.push(format!(
                        "[dry-run] Executaria `npx -y skills update` (inclui '{}' de {})",
                        skill.name, source
                    ));
                    continue;
                }
                let out = Command::new(NPX_CMD)
                    .args(["-y", "skills", "update"])
                    .output()
                    .context("Falha ao executar npx skills update")?;
                if out.status.success() {
                    report.push("✓ `npx skills update` concluído com sucesso".to_string());
                } else {
                    let err = String::from_utf8_lossy(&out.stderr);
                    report.push(format!("✗ `npx skills update` falhou: {}", err.trim()));
                }
            }

            SkillSource::CompanionGitClone {
                repo_dir,
                skill_subpath,
                ..
            } => {
                if dry_run {
                    report.push(format!(
                        "[dry-run] Executaria `git -C {} pull --ff-only` e sincronizaria '{}'",
                        shorten_home(repo_dir, &config.home_dir),
                        skill.name
                    ));
                    continue;
                }
                if !pulled_repos.contains(repo_dir) {
                    let out = Command::new("git")
                        .args(["-C", &repo_dir.to_string_lossy(), "pull", "--ff-only"])
                        .output()
                        .context("Falha ao executar git pull")?;
                    if !out.status.success() {
                        let err = String::from_utf8_lossy(&out.stderr);
                        bail!(
                            "git pull falhou em {}: {}",
                            repo_dir.display(),
                            err.trim()
                        );
                    }
                    pulled_repos.insert(repo_dir.clone());
                    report.push(format!(
                        "✓ Atualizado repositório {}",
                        shorten_home(repo_dir, &config.home_dir)
                    ));
                }

                let src_skill_dir = repo_dir.join(skill_subpath);
                let src_canon =
                    fs::canonicalize(&src_skill_dir).unwrap_or_else(|_| src_skill_dir.clone());
                if src_canon != skill.canonical_path {
                    copy_dir_clean(&src_skill_dir, &skill.canonical_path)?;
                    report.push(format!(
                        "✓ Sincronizada '{}' -> {}",
                        skill.name,
                        shorten_home(&skill.canonical_path, &config.home_dir)
                    ));
                }
            }

            SkillSource::VendoredGithub { repo, url, .. } => {
                if dry_run {
                    report.push(format!(
                        "[dry-run] Baixaria versão mais recente de {} para {}",
                        repo,
                        shorten_home(&skill.canonical_path, &config.home_dir)
                    ));
                    continue;
                }
                let tmp = TempDir::new()?;
                let clone_dir = tmp.path().join("repo");
                let git_url = if url.ends_with(".git") {
                    url.clone()
                } else {
                    format!("{url}.git")
                };
                let out = Command::new("git")
                    .args(["clone", "--depth", "1", &git_url, &clone_dir.to_string_lossy()])
                    .output()?;
                if !out.status.success() {
                    bail!(
                        "git clone falhou para {}: {}",
                        git_url,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                copy_dir_clean(&clone_dir, &skill.canonical_path)?;
                report.push(format!(
                    "✓ Atualizada '{}' a partir de {}",
                    skill.name, repo
                ));
            }

            SkillSource::ManagedBinary {
                tool_name,
                binary_path,
                ..
            } => {
                if upgraded_binaries.contains(binary_path) {
                    continue;
                }
                upgraded_binaries.insert(binary_path.clone());

                if dry_run {
                    report.push(format!(
                        "[dry-run] Executaria `{} upgrade` e `{} install-skills --scope global --agent claude-code`",
                        shorten_home(binary_path, &config.home_dir),
                        tool_name
                    ));
                    continue;
                }

                let up = Command::new(binary_path)
                    .arg("upgrade")
                    .output()
                    .with_context(|| format!("Falha ao executar {} upgrade", tool_name))?;
                if !up.status.success() {
                    let err = String::from_utf8_lossy(&up.stderr);
                    let out = String::from_utf8_lossy(&up.stdout);
                    bail!(
                        "{} upgrade falhou: {} {}",
                        tool_name,
                        err.trim(),
                        out.trim()
                    );
                }
                report.push(format!("✓ Binário `{tool_name}` atualizado com sucesso"));

                let refresh = Command::new(binary_path)
                    .args(["install-skills", "--scope", "global", "--agent", "claude-code"])
                    .output()
                    .with_context(|| format!("Falha ao atualizar skills de {tool_name}"))?;
                if !refresh.status.success() {
                    let err = String::from_utf8_lossy(&refresh.stderr);
                    bail!("{} install-skills falhou: {}", tool_name, err.trim());
                }
                report.push(format!(
                    "✓ Skills gerenciadas por `{tool_name}` atualizadas em ~/.claude/skills"
                ));
            }

            SkillSource::PersonalGitRepo { repo_dir, .. } => {
                if pulled_repos.contains(repo_dir) {
                    continue;
                }
                pulled_repos.insert(repo_dir.clone());
                if dry_run {
                    report.push(format!(
                        "[dry-run] Executaria `git -C {} pull --ff-only`",
                        shorten_home(repo_dir, &config.home_dir)
                    ));
                    continue;
                }
                let out = Command::new("git")
                    .args(["-C", &repo_dir.to_string_lossy(), "pull", "--ff-only"])
                    .output()?;
                if !out.status.success() {
                    bail!(
                        "git pull falhou em {}: {}",
                        repo_dir.display(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                report.push(format!(
                    "✓ Repositório pessoal {} atualizado",
                    shorten_home(repo_dir, &config.home_dir)
                ));
            }

            _ => {
                report.push(format!(
                    "• '{}' não possui origem atualizável automaticamente ({})",
                    skill.name,
                    skill.source.short_label()
                ));
            }
        }
    }

    Ok(report)
}

fn update_registry_skill(
    config: &AppConfig,
    skill: &SkillRecord,
    url: &str,
    subpath: &str,
    targets: &[AgentTarget],
) -> Result<ActionReport> {
    let mut report = ActionReport::new();
    let tmp = TempDir::new()?;
    let clone_dir = tmp.path().join("repo");
    let out = Command::new("git")
        .args(["clone", "--depth", "1", url, &clone_dir.to_string_lossy()])
        .output()?;
    if !out.status.success() {
        bail!(
            "git clone falhou para {}: {}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let commit_out = Command::new("git")
        .args(["-C", &clone_dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()?;
    let commit = String::from_utf8_lossy(&commit_out.stdout).trim().to_string();

    let src = if subpath.is_empty() || subpath == "." {
        clone_dir.clone()
    } else {
        clone_dir.join(subpath)
    };
    if !src.join("SKILL.md").is_file() {
        bail!("SKILL.md não encontrado em {subpath} dentro de {url}");
    }

    copy_dir_clean(&src, &skill.canonical_path)?;

    let mut registry = config.load_registry();
    if let Some(entry) = registry.skills.get_mut(&skill.name) {
        entry.commit = commit.clone();
        entry.updated_at = Utc::now();
        config.save_registry(&registry)?;
    }

    report.push(format!(
        "✓ Atualizada '{}' para commit {}",
        skill.name,
        &commit[..commit.len().min(7)]
    ));
    link_single_skill(
        config,
        &skill.name,
        &skill.canonical_path,
        targets,
        false,
        &mut report,
    )?;
    Ok(report)
}

pub fn uninstall_skill(
    config: &AppConfig,
    name: &str,
    dry_run: bool,
) -> Result<ActionReport> {
    let mut report = ActionReport::new();
    let unified_path = config.unified_skills_dir().join(name);
    if !unified_path.exists() {
        bail!(
            "Skill '{}' não encontrada em {}. Por segurança, `uninstall` só remove skills instaladas na raiz unificada (~/.agents/skills).",
            name,
            shorten_home(&config.unified_skills_dir(), &config.home_dir)
        );
    }

    let canon = fs::canonicalize(&unified_path).unwrap_or_else(|_| unified_path.clone());

    // Remove symlinks in agent directories pointing to this skill
    for target in AgentTarget::default_symlink_targets() {
        let link = target.dir(&config.home_dir).join(name);
        if let Ok(meta) = fs::symlink_metadata(&link) {
            if is_link_metadata(&meta) {
                let points_here = fs::canonicalize(&link)
                    .map(|p| p == canon)
                    .unwrap_or(false);
                if points_here {
                    if dry_run {
                        report.push(format!(
                            "[dry-run] Removeria symlink {}",
                            shorten_home(&link, &config.home_dir)
                        ));
                    } else {
                        remove_dir_link(&link)?;
                        report.push(format!(
                            "✓ Symlink removido: {}",
                            shorten_home(&link, &config.home_dir)
                        ));
                    }
                }
            }
        }
    }

    if dry_run {
        report.push(format!(
            "[dry-run] Removeria pasta {}",
            shorten_home(&unified_path, &config.home_dir)
        ));
    } else {
        fs::remove_dir_all(&unified_path)?;
        report.push(format!(
            "✓ Removida pasta {}",
            shorten_home(&unified_path, &config.home_dir)
        ));

        let mut registry = config.load_registry();
        if registry.skills.remove(name).is_some() {
            config.save_registry(&registry)?;
        }
    }

    Ok(report)
}

fn discover_skills_in_dir(root: &Path) -> Vec<(String, PathBuf, String)> {
    let mut results = Vec::new();

    // If root itself has SKILL.md, treat it as a single-skill repo
    if root.join("SKILL.md").is_file() {
        let content = fs::read_to_string(root.join("SKILL.md")).unwrap_or_default();
        let (fm_name, _) = parse_frontmatter(&content);
        let name = fm_name.unwrap_or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("skill")
                .to_string()
        });
        results.push((name, root.to_path_buf(), ".".to_string()));
        return results;
    }

    let walker = WalkDir::new(root).max_depth(4).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !matches!(
            name.as_ref(),
            ".git" | "node_modules" | "target" | "tests" | "test" | "evals" | "examples"
        )
    });

    for entry in walker.flatten() {
        if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
            if let Some(dir) = entry.path().parent() {
                let content = fs::read_to_string(entry.path()).unwrap_or_default();
                let (fm_name, _) = parse_frontmatter(&content);
                let name = fm_name.unwrap_or_else(|| {
                    dir.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("skill")
                        .to_string()
                });
                let subpath = dir
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string());
                results.push((name, dir.to_path_buf(), subpath));
            }
        }
    }

    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

fn filter_discovered(
    discovered: Vec<(String, PathBuf, String)>,
    want: Option<&str>,
) -> Result<Vec<(String, PathBuf, String)>> {
    let Some(target) = want else {
        return Ok(discovered);
    };
    let matched: Vec<_> = discovered
        .iter()
        .filter(|(name, dir, _)| {
            name.eq_ignore_ascii_case(target)
                || dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.eq_ignore_ascii_case(target))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();

    if matched.is_empty() {
        let avail: Vec<&str> = discovered.iter().map(|(n, _, _)| n.as_str()).collect();
        return Err(anyhow!(
            "Skill '{}' não encontrada. Disponíveis no repositório: {}",
            target,
            avail.join(", ")
        ));
    }
    Ok(matched)
}

fn copy_dir_clean(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;

    for entry in WalkDir::new(src).into_iter().filter_entry(|e| {
        e.file_name() != ".git" && e.file_name() != ".DS_Store"
    }) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_symlink() {
            let link_target = fs::read_link(entry.path())?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(link_target, &target)?;
            }
            #[cfg(windows)]
            {
                if entry.path().is_dir() {
                    let _ = create_dir_link(&link_target, &target);
                } else if entry.path().is_file() {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let _ = fs::copy(entry.path(), &target);
                }
            }
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_install_source_github_shorthand() {
        let parsed = parse_install_source("vercel-labs/skills@find-skills").unwrap();
        match parsed {
            InstallSource::GitRepo {
                url,
                slug,
                inline_skill,
            } => {
                assert_eq!(url, "https://github.com/vercel-labs/skills.git");
                assert_eq!(slug, "vercel-labs/skills");
                assert_eq!(inline_skill.as_deref(), Some("find-skills"));
            }
            _ => panic!("expected GitRepo"),
        }
    }
}
