use crate::config::{parse_frontmatter, AppConfig};
use crate::models::{
    extract_github_slug, AgentTarget, NpxSkillLockFile, RegistryFile, SkillRecord, SkillSource,
    UpdateStatus,
};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
struct CompanionRepoInfo {
    repo_dir: PathBuf,
    remote_url: String,
    local_commit: String,
    skill_subpath: PathBuf,
}

pub fn scan_all_skills(config: &AppConfig) -> Result<Vec<SkillRecord>> {
    let registry = config.load_registry();
    let npx_lock = config.load_npx_lock();

    let personal_repo_dir = config.unified_skills_dir();
    let personal_repo_info = inspect_git_repo(&personal_repo_dir);
    let personal_tracked_dirs = if personal_repo_info.is_some() {
        git_tracked_top_dirs(&personal_repo_dir)
    } else {
        HashSet::new()
    };

    let companion_repos = discover_companion_git_clones(&config.home_dir);
    let ai_memory_info = detect_ai_memory_binary(&config.home_dir);

    let mut records_by_canon: BTreeMap<PathBuf, SkillRecord> = BTreeMap::new();

    // 1. Scan standard agent skill roots
    for target in [
        AgentTarget::Agents,
        AgentTarget::Claude,
        AgentTarget::Antigravity,
        AgentTarget::Codex,
    ] {
        let root = target.dir(&config.home_dir);
        if !root.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                if fname_str.starts_with('.') || fname_str == "synced" {
                    continue;
                }
                let skill_md = entry_path.join("SKILL.md");
                if !skill_md.is_file() {
                    continue;
                }
                register_discovered_skill(
                    &mut records_by_canon,
                    &entry_path,
                    Some(target),
                    None,
                    &registry,
                    &npx_lock,
                    &personal_repo_dir,
                    personal_repo_info.as_ref(),
                    &personal_tracked_dirs,
                    &companion_repos,
                    ai_memory_info.as_ref(),
                );
            }
        }
    }

    // 2. Scan Claude cloud-synced skills (~/.claude/skills/synced/<bucket>/<skill>/SKILL.md)
    let claude_synced = config
        .home_dir
        .join(".claude")
        .join("skills")
        .join("synced");
    if claude_synced.is_dir() {
        for entry in WalkDir::new(&claude_synced)
            .min_depth(2)
            .max_depth(3)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
                if let Some(skill_dir) = entry.path().parent() {
                    register_discovered_skill(
                        &mut records_by_canon,
                        skill_dir,
                        Some(AgentTarget::Claude),
                        Some(SkillSource::CloudSynced {
                            provider: "Claude.ai".to_string(),
                        }),
                        &registry,
                        &npx_lock,
                        &personal_repo_dir,
                        personal_repo_info.as_ref(),
                        &personal_tracked_dirs,
                        &companion_repos,
                        ai_memory_info.as_ref(),
                    );
                }
            }
        }
    }

    // 3. Scan Antigravity built-in skills
    let agy_builtin = config
        .home_dir
        .join(".gemini")
        .join("antigravity")
        .join("builtin")
        .join("skills");
    if agy_builtin.is_dir() {
        if let Ok(entries) = fs::read_dir(&agy_builtin) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.join("SKILL.md").is_file() {
                    register_discovered_skill(
                        &mut records_by_canon,
                        &p,
                        Some(AgentTarget::Antigravity),
                        Some(SkillSource::SystemBuiltin {
                            app: "Antigravity".to_string(),
                        }),
                        &registry,
                        &npx_lock,
                        &personal_repo_dir,
                        personal_repo_info.as_ref(),
                        &personal_tracked_dirs,
                        &companion_repos,
                        ai_memory_info.as_ref(),
                    );
                }
            }
        }
    }

    // 4. Scan Codex .system skills
    let codex_system = config
        .home_dir
        .join(".codex")
        .join("skills")
        .join(".system");
    if codex_system.is_dir() {
        if let Ok(entries) = fs::read_dir(&codex_system) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.join("SKILL.md").is_file() {
                    register_discovered_skill(
                        &mut records_by_canon,
                        &p,
                        Some(AgentTarget::Codex),
                        Some(SkillSource::SystemBuiltin {
                            app: "Codex".to_string(),
                        }),
                        &registry,
                        &npx_lock,
                        &personal_repo_dir,
                        personal_repo_info.as_ref(),
                        &personal_tracked_dirs,
                        &companion_repos,
                        ai_memory_info.as_ref(),
                    );
                }
            }
        }
    }

    // 5. Scan local project skill folders (~/code, ~/Desktop/Projetos, ~/Projects, ~/Developer)
    let project_roots = [
        config.home_dir.join("code"),
        config.home_dir.join("Desktop").join("Projetos"),
        config.home_dir.join("Projects"),
        config.home_dir.join("Developer"),
    ];
    for base in project_roots {
        if !base.is_dir() {
            continue;
        }
        let walker = WalkDir::new(&base)
            .max_depth(4)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    ".git" | "node_modules" | "target" | ".venv" | "dist" | "build" | ".next"
                )
            });

        for entry in walker.flatten() {
            if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
                if let Some(skill_dir) = entry.path().parent() {
                    // Find project root (parent of `skills/` or `.agents/skills/`)
                    let project_dir = infer_project_root(skill_dir, &base);
                    register_discovered_skill(
                        &mut records_by_canon,
                        skill_dir,
                        None,
                        Some(SkillSource::ProjectLocal { project_dir }),
                        &registry,
                        &npx_lock,
                        &personal_repo_dir,
                        personal_repo_info.as_ref(),
                        &personal_tracked_dirs,
                        &companion_repos,
                        ai_memory_info.as_ref(),
                    );
                }
            }
        }
    }

    let mut records: Vec<SkillRecord> = records_by_canon.into_values().collect();
    for rec in &mut records {
        rec.linked_agents.sort();
        rec.linked_agents.dedup();
    }
    records.sort_by(|a, b| {
        a.source
            .sort_rank()
            .cmp(&b.source.sort_rank())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(records)
}

#[allow(clippy::too_many_arguments)]
fn register_discovered_skill(
    records_by_canon: &mut BTreeMap<PathBuf, SkillRecord>,
    entry_path: &Path,
    agent_target: Option<AgentTarget>,
    forced_source: Option<SkillSource>,
    registry: &RegistryFile,
    npx_lock: &NpxSkillLockFile,
    personal_repo_dir: &Path,
    personal_repo_info: Option<&(String, String)>,
    personal_tracked_dirs: &HashSet<String>,
    companion_repos: &HashMap<String, CompanionRepoInfo>,
    ai_memory_info: Option<&(PathBuf, Option<String>)>,
) {
    let canonical_dir = fs::canonicalize(entry_path).unwrap_or_else(|_| entry_path.to_path_buf());
    if let Some(existing) = records_by_canon.get_mut(&canonical_dir) {
        if !existing.discovered_paths.contains(&entry_path.to_path_buf()) {
            existing.discovered_paths.push(entry_path.to_path_buf());
        }
        if let Some(target) = agent_target {
            if !existing.linked_agents.contains(&target) {
                existing.linked_agents.push(target);
            }
        }
        return;
    }

    let skill_md_path = canonical_dir.join("SKILL.md");
    let content = fs::read_to_string(&skill_md_path).unwrap_or_default();
    let (fm_name, fm_desc) = parse_frontmatter(&content);
    let dir_name = canonical_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let name = fm_name.unwrap_or_else(|| dir_name.clone());
    let description = fm_desc.unwrap_or_default();

    let source = if let Some(forced) = forced_source {
        forced
    } else {
        classify_source(
            &name,
            &dir_name,
            &canonical_dir,
            &content,
            registry,
            npx_lock,
            personal_repo_dir,
            personal_repo_info,
            personal_tracked_dirs,
            companion_repos,
            ai_memory_info,
        )
    };

    let mut linked_agents = Vec::new();
    if let Some(t) = agent_target {
        linked_agents.push(t);
    }

    records_by_canon.insert(
        canonical_dir.clone(),
        SkillRecord {
            name,
            description,
            canonical_path: canonical_dir,
            skill_md_path,
            discovered_paths: vec![entry_path.to_path_buf()],
            linked_agents,
            source,
            status: UpdateStatus::Unchecked,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn classify_source(
    name: &str,
    dir_name: &str,
    canonical_dir: &Path,
    skill_md_content: &str,
    registry: &RegistryFile,
    npx_lock: &NpxSkillLockFile,
    personal_repo_dir: &Path,
    personal_repo_info: Option<&(String, String)>,
    personal_tracked_dirs: &HashSet<String>,
    companion_repos: &HashMap<String, CompanionRepoInfo>,
    ai_memory_info: Option<&(PathBuf, Option<String>)>,
) -> SkillSource {
    // 1. Managed directly by skillctl registry
    if let Some(reg) = registry.skills.get(name).or_else(|| registry.skills.get(dir_name)) {
        return SkillSource::SkillctlRegistry {
            repo: reg.repo.clone(),
            url: reg.url.clone(),
            subpath: reg.subpath.clone(),
            commit: reg.commit.clone(),
            branch: reg.branch.clone(),
        };
    }

    // 2. Managed by `npx skills` (.skill-lock.json)
    if let Some(lock) = npx_lock
        .skills
        .get(name)
        .or_else(|| npx_lock.skills.get(dir_name))
    {
        return SkillSource::NpxSkillLock {
            source: lock.source.clone(),
            source_url: lock.source_url.clone(),
            skill_path: lock.skill_path.clone(),
            folder_hash: lock.skill_folder_hash.clone(),
            commit: lock.commit.clone(),
        };
    }

    // 3. Managed by ai-memory binary
    if skill_md_content.contains("<!-- ai-memory-managed:") {
        if let Some((bin_path, version)) = ai_memory_info {
            return SkillSource::ManagedBinary {
                tool_name: "ai-memory".to_string(),
                binary_path: bin_path.clone(),
                github_repo: "akitaonrails/ai-memory".to_string(),
                installed_version: version.clone(),
            };
        }
    }

    // 4. Skill directory itself is a git repo
    if canonical_dir.join(".git").exists() {
        if let Some((remote_url, local_commit)) = inspect_git_repo(canonical_dir) {
            return SkillSource::CompanionGitClone {
                repo_dir: canonical_dir.to_path_buf(),
                remote_url,
                local_commit,
                skill_subpath: PathBuf::from("."),
            };
        }
    }

    // 5. Companion git clone on disk (e.g. ~/unslop -> ~/.claude/skills/unslop)
    if let Some(comp) = companion_repos
        .get(name)
        .or_else(|| companion_repos.get(dir_name))
    {
        return SkillSource::CompanionGitClone {
            repo_dir: comp.repo_dir.clone(),
            remote_url: comp.remote_url.clone(),
            local_commit: comp.local_commit.clone(),
            skill_subpath: comp.skill_subpath.clone(),
        };
    }

    // 6. Vendored third-party GitHub skill (e.g. book-to-skill inside ~/.agents/skills)
    if let Some((repo_slug, repo_url, local_ver)) = detect_vendored_github(canonical_dir) {
        let is_same_as_personal = personal_repo_info
            .and_then(|(url, _)| extract_github_slug(url))
            .map(|p| p.eq_ignore_ascii_case(&repo_slug))
            .unwrap_or(false);
        if !is_same_as_personal {
            return SkillSource::VendoredGithub {
                repo: repo_slug,
                url: repo_url,
                local_version: local_ver,
            };
        }
    }

    // 7. Tracked inside personal ~/.agents/skills git repo
    if let (Ok(canon_personal), Some((remote_url, local_commit))) =
        (fs::canonicalize(personal_repo_dir), personal_repo_info)
    {
        if canonical_dir.parent() == Some(canon_personal.as_path())
            && personal_tracked_dirs.contains(dir_name)
        {
            return SkillSource::PersonalGitRepo {
                repo_dir: personal_repo_dir.to_path_buf(),
                remote_url: remote_url.clone(),
                local_commit: local_commit.clone(),
            };
        }
    }

    SkillSource::Manual
}

fn infer_project_root(skill_dir: &Path, base: &Path) -> PathBuf {
    let mut cur = skill_dir;
    while let Some(parent) = cur.parent() {
        if parent == base {
            return cur.to_path_buf();
        }
        if parent.join(".git").exists() {
            return parent.to_path_buf();
        }
        cur = parent;
    }
    skill_dir
        .parent()
        .unwrap_or(skill_dir)
        .to_path_buf()
}

pub fn inspect_git_repo(dir: &Path) -> Option<(String, String)> {
    if !dir.join(".git").exists() {
        return None;
    }
    let remote = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;

    let head = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;

    if remote.is_empty() || head.is_empty() {
        None
    } else {
        Some((remote, head))
    }
}

fn git_tracked_top_dirs(repo_dir: &Path) -> HashSet<String> {
    let mut tracked = HashSet::new();
    let out = Command::new("git")
        .args(["-C", &repo_dir.to_string_lossy(), "ls-files"])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some((top, _)) = line.split_once('/') {
                    tracked.insert(top.to_string());
                }
            }
        }
    }
    tracked
}

fn discover_companion_git_clones(home: &Path) -> HashMap<String, CompanionRepoInfo> {
    let mut map = HashMap::new();
    let mut candidate_dirs = Vec::new();

    // Direct non-hidden directories in $HOME (like ~/unslop)
    if let Ok(entries) = fs::read_dir(home) {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.')
                || matches!(
                    s.as_ref(),
                    "Library" | "Applications" | "Movies" | "Music" | "Pictures" | "Public" | "Downloads"
                )
            {
                continue;
            }
            if p.is_dir() && p.join(".git").exists() {
                candidate_dirs.push(p);
            }
        }
    }

    // Direct repos inside ~/code
    let code_dir = home.join("code");
    if let Ok(entries) = fs::read_dir(&code_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join(".git").exists() {
                candidate_dirs.push(p);
            }
        }
    }

    for repo_dir in candidate_dirs {
        let Some((remote_url, local_commit)) = inspect_git_repo(&repo_dir) else {
            continue;
        };

        // Check `skills/<name>/SKILL.md`
        let skills_sub = repo_dir.join("skills");
        if skills_sub.is_dir() {
            if let Ok(entries) = fs::read_dir(&skills_sub) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.join("SKILL.md").is_file() {
                        let name = p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default()
                            .to_string();
                        if !name.is_empty() {
                            map.insert(
                                name.clone(),
                                CompanionRepoInfo {
                                    repo_dir: repo_dir.clone(),
                                    remote_url: remote_url.clone(),
                                    local_commit: local_commit.clone(),
                                    skill_subpath: PathBuf::from("skills").join(&name),
                                },
                            );
                        }
                    }
                }
            }
        }

        // Check root `SKILL.md`
        if repo_dir.join("SKILL.md").is_file() {
            let content = fs::read_to_string(repo_dir.join("SKILL.md")).unwrap_or_default();
            let (fm_name, _) = parse_frontmatter(&content);
            let name = fm_name.unwrap_or_else(|| {
                repo_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string()
            });
            if !name.is_empty() {
                map.entry(name).or_insert(CompanionRepoInfo {
                    repo_dir: repo_dir.clone(),
                    remote_url: remote_url.clone(),
                    local_commit: local_commit.clone(),
                    skill_subpath: PathBuf::from("."),
                });
            }
        }
    }

    map
}

fn detect_ai_memory_binary(home: &Path) -> Option<(PathBuf, Option<String>)> {
    #[cfg(windows)]
    let candidates: Vec<PathBuf> = {
        let mut c = vec![home
            .join(".local")
            .join("lib")
            .join("ai-memory")
            .join("ai-memory.exe")];
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            c.push(PathBuf::from(&local_app).join("ai-memory").join("ai-memory.exe"));
            c.push(
                PathBuf::from(&local_app)
                    .join("Programs")
                    .join("ai-memory")
                    .join("ai-memory.exe"),
            );
        }
        c
    };

    #[cfg(not(windows))]
    let candidates: Vec<PathBuf> = vec![home
        .join(".local")
        .join("lib")
        .join("ai-memory")
        .join("ai-memory")];

    let bin_path = if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        found
    } else {
        #[cfg(windows)]
        let which_cmd = "where";
        #[cfg(not(windows))]
        let which_cmd = "which";

        let which_out = Command::new(which_cmd).arg("ai-memory").output().ok()?;
        if !which_out.status.success() {
            return None;
        }
        let first_line = String::from_utf8_lossy(&which_out.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        PathBuf::from(first_line)
    };

    let ver_out = Command::new(&bin_path).arg("--version").output().ok();
    let version = ver_out.and_then(|o| {
        if o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout);
            text.split_whitespace().last().map(|s| s.trim().to_string())
        } else {
            None
        }
    });

    Some((bin_path, version))
}

fn detect_vendored_github(dir: &Path) -> Option<(String, String, Option<String>)> {
    let mut local_version: Option<String> = None;

    // Try pyproject.toml for version
    if let Ok(pyproject) = fs::read_to_string(dir.join("pyproject.toml")) {
        for line in pyproject.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("version") {
                if let Some((_, val)) = rest.split_once('=') {
                    let v = val.trim().trim_matches('"').trim_matches('\'');
                    if !v.is_empty() {
                        local_version = Some(v.to_string());
                        break;
                    }
                }
            }
        }
    }

    // Try CITATION.cff first (explicit repository-code)
    if let Ok(citation) = fs::read_to_string(dir.join("CITATION.cff")) {
        for line in citation.lines() {
            if line.contains("repository-code:") {
                if let Some(slug) = extract_github_slug(line) {
                    let url = format!("https://github.com/{slug}");
                    return Some((slug, url, local_version));
                }
            }
        }
    }

    // Try README.md for GitHub release badge or repo link
    if let Ok(readme) = fs::read_to_string(dir.join("README.md")) {
        for token in readme.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '(' || c == ')' || c == '<' || c == '>') {
            if token.contains("github.com/") {
                if let Some(slug) = extract_github_slug(token) {
                    // Prefer repo whose name matches directory name
                    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if slug.to_lowercase().ends_with(&format!("/{}", dir_name.to_lowercase())) {
                        let url = format!("https://github.com/{slug}");
                        return Some((slug, url, local_version));
                    }
                }
            }
        }
    }

    None
}
