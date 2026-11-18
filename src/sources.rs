use crate::models::{extract_github_slug, SkillRecord, SkillSource, UpdateStatus};
use futures::stream::{self, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Deserialize)]
struct GithubRepoMeta {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GithubTreeResponse {
    sha: String,
    tree: Vec<GithubTreeEntry>,
}

#[derive(Debug, Deserialize)]
struct GithubTreeEntry {
    path: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubCompareResponse {
    ahead_by: usize,
    commits: Vec<GithubCommitItem>,
}

#[derive(Debug, Deserialize)]
struct GithubCommitItem {
    sha: String,
    commit: GithubCommitInner,
}

#[derive(Debug, Deserialize)]
struct GithubCommitInner {
    message: String,
    author: Option<GithubAuthor>,
}

#[derive(Debug, Deserialize)]
struct GithubAuthor {
    date: Option<String>,
}

#[derive(Default)]
struct CheckCache {
    trees: HashMap<String, (String, String, HashMap<String, String>)>, // repo -> (branch, tree_sha, path->sha)
    releases: HashMap<String, (String, String)>,                       // repo -> (tag, date)
    latest_commits: HashMap<String, (String, String, String)>,         // repo -> (sha, date, msg)
}

pub struct UpdateChecker {
    client: reqwest::Client,
    cache: Arc<Mutex<CheckCache>>,
}

impl UpdateChecker {
    pub fn new() -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("skillctl/0.1.0"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );

        if let Some(token) = resolve_github_token() {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to construct HTTP client");

        Self {
            client,
            cache: Arc::new(Mutex::new(CheckCache::default())),
        }
    }

    pub async fn check_all(&self, skills: &mut [SkillRecord]) {
        let items: Vec<(usize, SkillRecord)> = skills.iter().cloned().enumerate().collect();
        let checked: Vec<(usize, UpdateStatus)> = stream::iter(items)
            .map(|(idx, skill)| async move {
                let status = self.check_one(&skill).await;
                (idx, status)
            })
            .buffer_unordered(8)
            .collect()
            .await;

        for (idx, status) in checked {
            skills[idx].status = status;
        }
    }

    pub async fn check_one(&self, skill: &SkillRecord) -> UpdateStatus {
        match &skill.source {
            SkillSource::SkillctlRegistry {
                repo,
                url,
                commit,
                branch,
                ..
            } => {
                self.check_git_remote_commit(url, repo, commit, branch.as_deref())
                    .await
            }

            SkillSource::NpxSkillLock {
                source,
                skill_path,
                folder_hash,
                commit,
                ..
            } => {
                self.check_npx_lock_skill(source, skill_path, folder_hash, commit.as_deref())
                    .await
            }

            SkillSource::CompanionGitClone {
                repo_dir,
                remote_url,
                local_commit,
                skill_subpath,
            } => {
                let slug = extract_github_slug(remote_url).unwrap_or_else(|| remote_url.clone());
                let mut status = self
                    .check_git_remote_commit(remote_url, &slug, local_commit, None)
                    .await;

                // Also verify whether the installed copy is out of sync with the local clone
                if matches!(status, UpdateStatus::UpToDate { .. }) {
                    let clone_skill_md = repo_dir.join(skill_subpath).join("SKILL.md");
                    if clone_skill_md.is_file() && clone_skill_md != skill.skill_md_path {
                        let a = fs::read(&clone_skill_md).ok();
                        let b = fs::read(&skill.skill_md_path).ok();
                        if a != b {
                            status = UpdateStatus::UpdateAvailable {
                                current: short_sha(local_commit),
                                latest: short_sha(local_commit),
                                summary: "Clone local está à frente da cópia instalada".to_string(),
                                commits: vec!["Sincronizar cópia do clone local para a pasta instalada".to_string()],
                            };
                        }
                    }
                }
                status
            }

            SkillSource::VendoredGithub {
                repo,
                local_version,
                ..
            } => {
                self.check_vendored_github(repo, &skill.skill_md_path, local_version.as_deref())
                    .await
            }

            SkillSource::ManagedBinary {
                tool_name,
                github_repo,
                installed_version,
                ..
            } => {
                self.check_managed_binary(tool_name, github_repo, installed_version.as_deref())
                    .await
            }

            SkillSource::PersonalGitRepo {
                repo_dir,
                remote_url,
                local_commit,
            } => {
                let slug = extract_github_slug(remote_url).unwrap_or_else(|| "origin".to_string());
                self.check_git_remote_commit(remote_url, &slug, local_commit, None)
                    .await
                    .with_dirty_hint(repo_dir)
            }

            SkillSource::CloudSynced { provider } => UpdateStatus::NotTrackable {
                reason: format!("Sincronizada automaticamente via {provider}"),
            },

            SkillSource::SystemBuiltin { app } => UpdateStatus::NotTrackable {
                reason: format!("Embutida no {app} (atualizada junto com o app)"),
            },

            SkillSource::ProjectLocal { .. } => UpdateStatus::NotTrackable {
                reason: "Skill autoral/local de projeto".to_string(),
            },

            SkillSource::Manual => UpdateStatus::NotTrackable {
                reason: "Instalada manualmente (sem origem upstream)".to_string(),
            },
        }
    }

    async fn check_git_remote_commit(
        &self,
        url: &str,
        slug: &str,
        local_commit: &str,
        branch: Option<&str>,
    ) -> UpdateStatus {
        let url_owned = url.to_string();
        let ref_spec = branch
            .map(|b| format!("refs/heads/{b}"))
            .unwrap_or_else(|| "HEAD".to_string());

        let remote_sha = tokio::task::spawn_blocking(move || git_ls_remote(&url_owned, &ref_spec))
            .await
            .ok()
            .flatten();

        let Some(remote_sha) = remote_sha else {
            return UpdateStatus::Error {
                message: format!("Falha ao consultar git remote ({slug})"),
            };
        };

        if remote_sha.eq_ignore_ascii_case(local_commit) {
            return UpdateStatus::UpToDate {
                detail: format!("commit {} (em dia)", short_sha(local_commit)),
            };
        }

        // Fetch commit summary via GitHub compare API if it's a GitHub repo
        let mut commits = Vec::new();
        let mut summary = format!(
            "{} -> {}",
            short_sha(local_commit),
            short_sha(&remote_sha)
        );
        if slug.contains('/') && !slug.contains("://") {
            let api_url = format!(
                "https://api.github.com/repos/{slug}/compare/{local_commit}...{remote_sha}"
            );
            if let Ok(resp) = self.client.get(&api_url).send().await {
                if resp.status().is_success() {
                    if let Ok(cmp) = resp.json::<GithubCompareResponse>().await {
                        summary = format!("{} commit(s) atrás", cmp.ahead_by);
                        commits = cmp
                            .commits
                            .into_iter()
                            .rev()
                            .take(6)
                            .map(|c| {
                                let date = c
                                    .commit
                                    .author
                                    .and_then(|a| a.date)
                                    .map(|d| d.chars().take(10).collect::<String>())
                                    .unwrap_or_default();
                                let first_line = c
                                    .commit
                                    .message
                                    .lines()
                                    .next()
                                    .unwrap_or_default()
                                    .to_string();
                                format!("{} {} {}", short_sha(&c.sha), date, first_line)
                            })
                            .collect();
                    }
                }
            }
        }

        UpdateStatus::UpdateAvailable {
            current: short_sha(local_commit),
            latest: short_sha(&remote_sha),
            summary,
            commits,
        }
    }

    async fn check_npx_lock_skill(
        &self,
        source_repo: &str,
        skill_path: &str,
        folder_hash: &str,
        lock_commit: Option<&str>,
    ) -> UpdateStatus {
        let Some((branch, _tree_sha, tree_map)) = self.get_repo_tree(source_repo).await else {
            return UpdateStatus::Error {
                message: format!("Falha ao consultar GitHub tree de {source_repo}"),
            };
        };

        let folder_path = Path::new(skill_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if let Some(remote_folder_sha) = tree_map.get(&folder_path) {
            if remote_folder_sha.eq_ignore_ascii_case(folder_hash) {
                let extra = lock_commit
                    .map(|c| format!("commit {}", short_sha(c)))
                    .unwrap_or_else(|| format!("tree {}", short_sha(folder_hash)));
                return UpdateStatus::UpToDate {
                    detail: format!("{extra} em {source_repo} ({branch})"),
                };
            } else {
                let latest_commit = self
                    .get_latest_commit(source_repo, &branch)
                    .await
                    .map(|(sha, date, msg)| format!("{} ({date}) {msg}", short_sha(&sha)));
                return UpdateStatus::UpdateAvailable {
                    current: short_sha(folder_hash),
                    latest: short_sha(remote_folder_sha),
                    summary: format!("Nova versão disponível em {source_repo}"),
                    commits: latest_commit.into_iter().collect(),
                };
            }
        }

        // Fallback: compare commit if lock has commit
        if let Some(c) = lock_commit {
            let url = format!("https://github.com/{source_repo}.git");
            return self
                .check_git_remote_commit(&url, source_repo, c, Some(&branch))
                .await;
        }

        UpdateStatus::Error {
            message: format!("Caminho '{folder_path}' não encontrado em {source_repo}"),
        }
    }

    async fn check_vendored_github(
        &self,
        repo: &str,
        local_skill_md: &Path,
        local_version: Option<&str>,
    ) -> UpdateStatus {
        let Some((branch, _tree_sha, tree_map)) = self.get_repo_tree(repo).await else {
            return UpdateStatus::Error {
                message: format!("Não foi possível consultar {repo} no GitHub"),
            };
        };

        let Ok(local_bytes) = fs::read(local_skill_md) else {
            return UpdateStatus::Error {
                message: "Erro ao ler SKILL.md local".to_string(),
            };
        };
        let local_blob_sha = git_blob_sha1(&local_bytes);

        // Locate SKILL.md in remote tree (root `SKILL.md` or `skills/*/SKILL.md`)
        let remote_sha = tree_map.get("SKILL.md").cloned().or_else(|| {
            tree_map
                .iter()
                .find(|(p, _)| p.ends_with("/SKILL.md"))
                .map(|(_, sha)| sha.clone())
        });

        let latest_commit = self.get_latest_commit(repo, &branch).await;

        if let Some(r_sha) = remote_sha {
            if r_sha.eq_ignore_ascii_case(&local_blob_sha) {
                let ver_label = latest_commit
                    .as_ref()
                    .map(|(sha, date, _)| format!("HEAD {} ({date})", short_sha(sha)))
                    .or_else(|| local_version.map(|v| format!("v{v}")))
                    .unwrap_or_else(|| "em dia".to_string());
                return UpdateStatus::UpToDate {
                    detail: format!("SKILL.md idêntico ao {ver_label} ({repo}/{branch})"),
                };
            } else {
                let (latest_label, commits) = match latest_commit {
                    Some((sha, date, msg)) => (
                        short_sha(&sha),
                        vec![format!("{} {} {}", short_sha(&sha), date, msg)],
                    ),
                    None => (short_sha(&r_sha), vec![]),
                };
                return UpdateStatus::UpdateAvailable {
                    current: local_version
                        .map(|v| format!("v{v}"))
                        .unwrap_or_else(|| short_sha(&local_blob_sha)),
                    latest: latest_label,
                    summary: format!("SKILL.md difere do HEAD de {repo} ({branch})"),
                    commits,
                };
            }
        }

        UpdateStatus::Error {
            message: format!("SKILL.md não encontrado em {repo}"),
        }
    }

    async fn check_managed_binary(
        &self,
        tool_name: &str,
        github_repo: &str,
        installed_version: Option<&str>,
    ) -> UpdateStatus {
        let Some((latest_tag, published_date)) = self.get_latest_release(github_repo).await else {
            return UpdateStatus::Error {
                message: format!("Falha ao consultar releases de {github_repo}"),
            };
        };

        let Some(installed) = installed_version else {
            return UpdateStatus::Error {
                message: format!("Não foi possível ler versão local de {tool_name}"),
            };
        };

        let cur_clean = installed.trim_start_matches('v');
        let lat_clean = latest_tag.trim_start_matches('v');

        if cur_clean == lat_clean {
            UpdateStatus::UpToDate {
                detail: format!("{tool_name} v{cur_clean} (última release)"),
            }
        } else {
            let mut commits = vec![format!(
                "Release {latest_tag} publicada em {published_date} ({github_repo})"
            )];

            // Check which skill files changed between versions
            let cmp_url = format!(
                "https://api.github.com/repos/{github_repo}/compare/v{cur_clean}...{latest_tag}"
            );
            if let Ok(resp) = self.client.get(&cmp_url).send().await {
                if resp.status().is_success() {
                    if let Ok(val) = resp.json::<serde_json::Value>().await {
                        if let Some(files) = val.get("files").and_then(|f| f.as_array()) {
                            let skill_files: Vec<String> = files
                                .iter()
                                .filter_map(|f| f.get("filename").and_then(|n| n.as_str()))
                                .filter(|n| n.contains("SKILL.md") || n.contains("routing_skills"))
                                .map(|s| format!("alterado: {s}"))
                                .take(5)
                                .collect();
                            commits.extend(skill_files);
                        }
                    }
                }
            }

            UpdateStatus::UpdateAvailable {
                current: format!("v{cur_clean}"),
                latest: latest_tag.clone(),
                summary: format!("{tool_name} v{cur_clean} -> {latest_tag}"),
                commits,
            }
        }
    }

    async fn get_repo_tree(&self, repo: &str) -> Option<(String, String, HashMap<String, String>)> {
        {
            let cache = self.cache.lock().await;
            if let Some(hit) = cache.trees.get(repo) {
                return Some(hit.clone());
            }
        }

        let meta_url = format!("https://api.github.com/repos/{repo}");
        let meta: GithubRepoMeta = self
            .client
            .get(&meta_url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;

        let tree_url = format!(
            "https://api.github.com/repos/{repo}/git/trees/{}?recursive=1",
            meta.default_branch
        );
        let tree_resp: GithubTreeResponse = self
            .client
            .get(&tree_url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;

        let mut map = HashMap::new();
        for item in tree_resp.tree {
            map.insert(item.path, item.sha);
        }

        let entry = (meta.default_branch, tree_resp.sha, map);
        let mut cache = self.cache.lock().await;
        cache.trees.insert(repo.to_string(), entry.clone());
        Some(entry)
    }

    async fn get_latest_release(&self, repo: &str) -> Option<(String, String)> {
        {
            let cache = self.cache.lock().await;
            if let Some(hit) = cache.releases.get(repo) {
                return Some(hit.clone());
            }
        }

        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let rel: GithubRelease = self
            .client
            .get(&url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;

        let date = rel
            .published_at
            .map(|d| d.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let val = (rel.tag_name, date);
        let mut cache = self.cache.lock().await;
        cache.releases.insert(repo.to_string(), val.clone());
        Some(val)
    }

    async fn get_latest_commit(&self, repo: &str, branch: &str) -> Option<(String, String, String)> {
        let key = format!("{repo}@{branch}");
        {
            let cache = self.cache.lock().await;
            if let Some(hit) = cache.latest_commits.get(&key) {
                return Some(hit.clone());
            }
        }

        let url = format!("https://api.github.com/repos/{repo}/commits?sha={branch}&per_page=1");
        let items: Vec<GithubCommitItem> = self
            .client
            .get(&url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;

        let first = items.into_iter().next()?;
        let date = first
            .commit
            .author
            .and_then(|a| a.date)
            .map(|d| d.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let msg = first
            .commit
            .message
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();

        let val = (first.sha, date, msg);
        let mut cache = self.cache.lock().await;
        cache.latest_commits.insert(key, val.clone());
        Some(val)
    }
}

impl UpdateStatus {
    fn with_dirty_hint(self, repo_dir: &Path) -> Self {
        if let UpdateStatus::UpToDate { mut detail } = self {
            let dirty = Command::new("git")
                .args(["-C", &repo_dir.to_string_lossy(), "status", "--porcelain"])
                .output()
                .ok()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            if dirty {
                detail.push_str(" (alterações locais não commitadas)");
            }
            UpdateStatus::UpToDate { detail }
        } else {
            self
        }
    }
}

pub fn git_ls_remote(url: &str, ref_spec: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["ls-remote", url, ref_spec])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(|s| s.to_string())
}

pub fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn resolve_github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        let trimmed = t.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if out.status.success() {
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_blob_sha1_matches_git() {
        // Known git hash-object for "hello\n"
        assert_eq!(
            git_blob_sha1(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
