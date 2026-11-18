use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTarget {
    Agents,
    Claude,
    Antigravity,
    Codex,
}

impl AgentTarget {
    pub fn default_symlink_targets() -> Vec<AgentTarget> {
        vec![
            AgentTarget::Claude,
            AgentTarget::Antigravity,
            AgentTarget::Codex,
        ]
    }

    pub fn dir(&self, home: &Path) -> PathBuf {
        match self {
            AgentTarget::Agents => home.join(".agents").join("skills"),
            AgentTarget::Claude => home.join(".claude").join("skills"),
            AgentTarget::Antigravity => home.join(".gemini").join("config").join("skills"),
            AgentTarget::Codex => home.join(".codex").join("skills"),
        }
    }

    pub fn short_code(&self) -> &'static str {
        match self {
            AgentTarget::Agents => "A",
            AgentTarget::Claude => "C",
            AgentTarget::Antigravity => "G",
            AgentTarget::Codex => "X",
        }
    }

    pub fn from_cli_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "agents" | "unified" => Some(AgentTarget::Agents),
            "claude" | "claude-code" => Some(AgentTarget::Claude),
            "antigravity" | "agy" | "gemini" => Some(AgentTarget::Antigravity),
            "codex" => Some(AgentTarget::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SkillSource {
    SkillctlRegistry {
        repo: String,
        url: String,
        subpath: String,
        commit: String,
        branch: Option<String>,
    },
    NpxSkillLock {
        source: String,
        source_url: String,
        skill_path: String,
        folder_hash: String,
        commit: Option<String>,
    },
    CompanionGitClone {
        repo_dir: PathBuf,
        remote_url: String,
        local_commit: String,
        skill_subpath: PathBuf,
    },
    VendoredGithub {
        repo: String,
        url: String,
        local_version: Option<String>,
    },
    ManagedBinary {
        tool_name: String,
        binary_path: PathBuf,
        github_repo: String,
        installed_version: Option<String>,
    },
    PersonalGitRepo {
        repo_dir: PathBuf,
        remote_url: String,
        local_commit: String,
    },
    CloudSynced {
        provider: String,
    },
    SystemBuiltin {
        app: String,
    },
    ProjectLocal {
        project_dir: PathBuf,
    },
    Manual,
}

impl SkillSource {
    pub fn short_label(&self) -> String {
        match self {
            SkillSource::SkillctlRegistry { repo, .. } => format!("skillctl ({repo})"),
            SkillSource::NpxSkillLock { source, .. } => format!("npx ({source})"),
            SkillSource::CompanionGitClone { remote_url, .. } => {
                let slug = extract_github_slug(remote_url).unwrap_or_else(|| remote_url.clone());
                format!("git ({slug})")
            }
            SkillSource::VendoredGithub { repo, .. } => format!("github ({repo})"),
            SkillSource::ManagedBinary { tool_name, .. } => format!("bin ({tool_name})"),
            SkillSource::PersonalGitRepo { remote_url, .. } => {
                let slug = extract_github_slug(remote_url).unwrap_or_else(|| "pessoal".into());
                format!("pessoal ({slug})")
            }
            SkillSource::CloudSynced { provider } => format!("sync ({provider})"),
            SkillSource::SystemBuiltin { app } => format!("sistema ({app})"),
            SkillSource::ProjectLocal { project_dir } => {
                let name = project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("projeto");
                format!("projeto ({name})")
            }
            SkillSource::Manual => "manual".to_string(),
        }
    }

    pub fn is_system_or_cloud(&self) -> bool {
        matches!(
            self,
            SkillSource::CloudSynced { .. } | SkillSource::SystemBuiltin { .. }
        )
    }

    pub fn can_update(&self) -> bool {
        matches!(
            self,
            SkillSource::SkillctlRegistry { .. }
                | SkillSource::NpxSkillLock { .. }
                | SkillSource::CompanionGitClone { .. }
                | SkillSource::VendoredGithub { .. }
                | SkillSource::ManagedBinary { .. }
                | SkillSource::PersonalGitRepo { .. }
        )
    }

    pub fn sort_rank(&self) -> u8 {
        match self {
            SkillSource::SkillctlRegistry { .. } => 0,
            SkillSource::NpxSkillLock { .. } => 1,
            SkillSource::CompanionGitClone { .. } => 2,
            SkillSource::ManagedBinary { .. } => 3,
            SkillSource::VendoredGithub { .. } => 4,
            SkillSource::PersonalGitRepo { .. } => 5,
            SkillSource::Manual => 6,
            SkillSource::ProjectLocal { .. } => 7,
            SkillSource::CloudSynced { .. } => 8,
            SkillSource::SystemBuiltin { .. } => 9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UpdateStatus {
    Unchecked,
    UpToDate {
        detail: String,
    },
    UpdateAvailable {
        current: String,
        latest: String,
        summary: String,
        commits: Vec<String>,
    },
    NotTrackable {
        reason: String,
    },
    Error {
        message: String,
    },
}

impl UpdateStatus {
    pub fn is_outdated(&self) -> bool {
        matches!(self, UpdateStatus::UpdateAvailable { .. })
    }

    pub fn is_up_to_date(&self) -> bool {
        matches!(self, UpdateStatus::UpToDate { .. })
    }

    pub fn badge(&self) -> &'static str {
        match self {
            UpdateStatus::Unchecked => "· PEND",
            UpdateStatus::UpToDate { .. } => "✓ OK",
            UpdateStatus::UpdateAvailable { .. } => "↑ UPDATE",
            UpdateStatus::NotTrackable { .. } => "• LOCAL",
            UpdateStatus::Error { .. } => "! ERRO",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub canonical_path: PathBuf,
    pub skill_md_path: PathBuf,
    pub discovered_paths: Vec<PathBuf>,
    pub linked_agents: Vec<AgentTarget>,
    pub source: SkillSource,
    pub status: UpdateStatus,
}

impl SkillRecord {
    pub fn agents_badge(&self) -> String {
        let all = [
            AgentTarget::Agents,
            AgentTarget::Claude,
            AgentTarget::Antigravity,
            AgentTarget::Codex,
        ];
        all.iter()
            .map(|t| {
                if self.linked_agents.contains(t) {
                    t.short_code()
                } else {
                    "-"
                }
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,
    pub skills: BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub name: String,
    pub repo: String,
    pub url: String,
    pub subpath: String,
    pub commit: String,
    pub branch: Option<String>,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NpxSkillLockFile {
    #[serde(default)]
    pub skills: BTreeMap<String, NpxSkillLockEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpxSkillLockEntry {
    pub source: String,
    pub source_url: String,
    pub skill_path: String,
    pub skill_folder_hash: String,
    pub commit: Option<String>,
}

pub fn extract_github_slug(url: &str) -> Option<String> {
    let cleaned = url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");

    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some(format!("{}/{}", parts[0], parts[1]));
        }
    }

    if let Some(idx) = cleaned.find("github.com/") {
        let rest = &cleaned[idx + "github.com/".len()..];
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 {
            let owner = parts[0].trim();
            let repo = parts[1]
                .trim_end_matches(".git")
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.');
            if !owner.is_empty() && !repo.is_empty() && owner != "sponsors" && owner != "topics" {
                return Some(format!("{owner}/{repo}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_github_slug() {
        assert_eq!(
            extract_github_slug("https://github.com/heydamir/unslop.git"),
            Some("heydamir/unslop".to_string())
        );
        assert_eq!(
            extract_github_slug("git@github.com:oliverduds/agent-skills.git"),
            Some("oliverduds/agent-skills".to_string())
        );
        assert_eq!(
            extract_github_slug("https://github.com/virgiliojr94/book-to-skill/releases"),
            Some("virgiliojr94/book-to-skill".to_string())
        );
        assert_eq!(
            extract_github_slug("https://github.com/sponsors/virgiliojr94"),
            None
        );
    }
}
