use crate::models::{NpxSkillLockFile, RegistryFile};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub home_dir: PathBuf,
}

impl AppConfig {
    pub fn discover() -> Result<Self> {
        let home_dir = dirs::home_dir().context("Não foi possível identificar o diretório HOME")?;
        Ok(Self { home_dir })
    }

    pub fn unified_skills_dir(&self) -> PathBuf {
        self.home_dir.join(".agents").join("skills")
    }

    pub fn registry_path(&self) -> PathBuf {
        self.home_dir.join(".agents").join("skillctl-registry.json")
    }

    pub fn npx_lock_path(&self) -> PathBuf {
        self.home_dir.join(".agents").join(".skill-lock.json")
    }

    pub fn load_registry(&self) -> RegistryFile {
        let path = self.registry_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| RegistryFile {
                version: 1,
                skills: Default::default(),
            }),
            Err(_) => RegistryFile {
                version: 1,
                skills: Default::default(),
            },
        }
    }

    pub fn save_registry(&self, registry: &RegistryFile) -> Result<()> {
        let path = self.registry_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut to_save = registry.clone();
        to_save.version = 1;
        let json = serde_json::to_string_pretty(&to_save)?;
        fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("Erro ao salvar {}", path.display()))
    }

    pub fn load_npx_lock(&self) -> NpxSkillLockFile {
        let path = self.npx_lock_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => NpxSkillLockFile::default(),
        }
    }
}

/// Parses `name` and `description` from SKILL.md YAML frontmatter cleanly without external YAML deps.
pub fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let trimmed = content.trim_start_matches('\u{feff}');
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        return (None, None);
    };
    if first.trim() != "---" {
        return (None, None);
    }

    let mut fm_lines = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        fm_lines.push(line);
    }
    if !closed {
        return (None, None);
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    let mut i = 0;
    while i < fm_lines.len() {
        let line = fm_lines[i];
        if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some((key, raw_val)) = line.split_once(':') {
            let key = key.trim();
            let val = raw_val.trim();
            if key == "name" || key == "description" {
                let parsed = if matches!(val, ">" | ">-" | "|" | "|-") {
                    let mut block = Vec::new();
                    i += 1;
                    while i < fm_lines.len() {
                        let next = fm_lines[i];
                        if next.starts_with(' ') || next.starts_with('\t') {
                            block.push(next.trim());
                            i += 1;
                        } else if next.trim().is_empty() {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    block.join(" ")
                } else {
                    i += 1;
                    unquote(val)
                };

                if key == "name" && !parsed.is_empty() {
                    name = Some(parsed);
                } else if key == "description" && !parsed.is_empty() {
                    description = Some(parsed);
                }
                continue;
            }
        }
        i += 1;
    }

    (name, description)
}

fn unquote(val: &str) -> String {
    let v = val.trim();
    if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
        if v.len() >= 2 {
            return v[1..v.len() - 1].replace("\\\"", "\"");
        }
    }
    v.to_string()
}

pub fn shorten_home(path: &Path, home: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(home) {
        format!("~/{}", rel.display())
    } else {
        path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_inline_and_block() {
        let md = r#"---
name: bento-ui-motion-grid
description: >-
  First line of description
  second line here.
---
# Body
"#;
        let (name, desc) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("bento-ui-motion-grid"));
        assert_eq!(
            desc.as_deref(),
            Some("First line of description second line here.")
        );
    }

    #[test]
    fn test_parse_frontmatter_quoted() {
        let md = "---\nname: \"book-to-skill\"\ndescription: \"Converts books into skills.\"\n---\n";
        let (name, desc) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("book-to-skill"));
        assert_eq!(desc.as_deref(), Some("Converts books into skills."));
    }
}
