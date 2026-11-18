use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrendingPeriod {
    Day,
    Week,
    Month,
    Hot,
    AllTime,
}

impl TrendingPeriod {
    pub fn label(self) -> &'static str {
        match self {
            TrendingPeriod::Day => "Dia (últimas 24h)",
            TrendingPeriod::Week => "Semana (últimos 7 dias)",
            TrendingPeriod::Month => "Mês (últimos 30 dias)",
            TrendingPeriod::Hot => "Em alta hoje (maior salto)",
            TrendingPeriod::AllTime => "Todos os tempos",
        }
    }

    pub fn from_cli_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "day" | "dia" | "24h" | "today" | "hoje" => Some(TrendingPeriod::Day),
            "week" | "semana" | "7d" | "weekly" => Some(TrendingPeriod::Week),
            "month" | "mes" | "mês" | "30d" | "monthly" => Some(TrendingPeriod::Month),
            "hot" | "alta" | "rising" => Some(TrendingPeriod::Hot),
            "all" | "all-time" | "alltime" | "total" => Some(TrendingPeriod::AllTime),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrendingSkill {
    pub rank: usize,
    pub name: String,
    pub skill_id: String,
    pub source: String,
    pub metric_value: u64,
    pub metric_label: String,
    pub total_installs: u64,
    pub is_official: bool,
    pub trend_sparkline: Option<String>,
}

impl TrendingSkill {
    pub fn install_spec(&self) -> String {
        format!("{}@{}", self.source, self.skill_id)
    }

    pub fn web_url(&self) -> String {
        format!("https://skills.sh/{}/{}", self.source, self.skill_id)
    }
}

#[derive(Debug, Deserialize)]
struct ApiListResponse {
    #[serde(default)]
    skills: Vec<ApiSkillItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSkillItem {
    source: String,
    skill_id: String,
    name: String,
    #[serde(default)]
    installs: u64,
    #[serde(default)]
    change: Option<i64>,
    #[serde(default)]
    weekly_installs: Option<Vec<u64>>,
    #[serde(default)]
    is_official: bool,
}

pub struct DiscoverClient {
    client: reqwest::Client,
}

impl DiscoverClient {
    pub fn new() -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("skillctl/0.2.0"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to construct HTTP client");

        Self { client }
    }

    async fn fetch_view_page(&self, view: &str, page: usize) -> Result<Vec<ApiSkillItem>> {
        let url = format!("https://skills.sh/api/skills/{view}/{page}");
        let resp: ApiListResponse = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Falha ao conectar em {url}"))?
            .error_for_status()
            .with_context(|| format!("Erro HTTP ao consultar {url}"))?
            .json()
            .await
            .with_context(|| format!("Resposta JSON inválida de {url}"))?;
        Ok(resp.skills)
    }

    pub async fn fetch_trending(
        &self,
        period: TrendingPeriod,
        limit: usize,
    ) -> Result<Vec<TrendingSkill>> {
        let mut out: Vec<TrendingSkill> = match period {
            TrendingPeriod::Day => {
                let items = self.fetch_view_page("trending", 0).await?;
                items
                    .into_iter()
                    .map(|it| {
                        let m = it.installs;
                        TrendingSkill {
                            rank: 0,
                            name: it.name,
                            skill_id: it.skill_id,
                            source: it.source,
                            metric_value: m,
                            metric_label: format!("{} / 24h", format_count(m)),
                            total_installs: m,
                            is_official: it.is_official,
                            trend_sparkline: None,
                        }
                    })
                    .collect()
            }

            TrendingPeriod::Hot => {
                let items = self.fetch_view_page("hot", 0).await?;
                items
                    .into_iter()
                    .map(|it| {
                        let ch = it.change.unwrap_or(0);
                        let m = ch.max(0) as u64;
                        let sign = if ch >= 0 { "+" } else { "" };
                        TrendingSkill {
                            rank: 0,
                            name: it.name,
                            skill_id: it.skill_id,
                            source: it.source,
                            metric_value: m,
                            metric_label: format!("{sign}{} hoje ({})", format_count(m), format_count(it.installs)),
                            total_installs: it.installs,
                            is_official: it.is_official,
                            trend_sparkline: None,
                        }
                    })
                    .collect()
            }

            TrendingPeriod::AllTime => {
                let items = self.fetch_view_page("all-time", 0).await?;
                items
                    .into_iter()
                    .map(|it| {
                        let spark = it.weekly_installs.as_deref().map(sparkline);
                        let m = it.installs;
                        TrendingSkill {
                            rank: 0,
                            name: it.name,
                            skill_id: it.skill_id,
                            source: it.source,
                            metric_value: m,
                            metric_label: format!("{} total", format_count(m)),
                            total_installs: m,
                            is_official: it.is_official,
                            trend_sparkline: spark,
                        }
                    })
                    .collect()
            }

            TrendingPeriod::Week | TrendingPeriod::Month => {
                // Fetch top 2 pages (400 skills) concurrently to get accurate weekly/monthly leaders
                let (p0, p1) = tokio::join!(
                    self.fetch_view_page("all-time", 0),
                    self.fetch_view_page("all-time", 1)
                );
                let mut combined = p0?;
                if let Ok(extra) = p1 {
                    combined.extend(extra);
                }

                let mut seen = HashSet::new();
                let mut ranked: Vec<TrendingSkill> = combined
                    .into_iter()
                    .filter(|it| seen.insert((it.source.clone(), it.skill_id.clone())))
                    .filter_map(|it| {
                        let weekly = it.weekly_installs?;
                        if weekly.is_empty() {
                            return None;
                        }
                        let (metric, suffix) = match period {
                            TrendingPeriod::Week => (*weekly.last().unwrap_or(&0), "sem"),
                            TrendingPeriod::Month => {
                                (weekly.iter().rev().take(4).sum::<u64>(), "mês")
                            }
                            _ => unreachable!(),
                        };
                        let spark = Some(sparkline(&weekly));
                        Some(TrendingSkill {
                            rank: 0,
                            name: it.name,
                            skill_id: it.skill_id,
                            source: it.source,
                            metric_value: metric,
                            metric_label: format!("{} / {suffix}", format_count(metric)),
                            total_installs: it.installs,
                            is_official: it.is_official,
                            trend_sparkline: spark,
                        })
                    })
                    .collect();

                ranked.sort_by(|a, b| {
                    b.metric_value
                        .cmp(&a.metric_value)
                        .then_with(|| b.total_installs.cmp(&a.total_installs))
                });
                ranked
            }
        };

        out.truncate(limit);
        for (idx, item) in out.iter_mut().enumerate() {
            item.rank = idx + 1;
        }
        Ok(out)
    }

    pub async fn search_skills(&self, query: &str, limit: usize) -> Result<Vec<TrendingSkill>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let resp: ApiListResponse = self
            .client
            .get("https://skills.sh/api/search")
            .query(&[("q", trimmed), ("limit", &limit.to_string())])
            .send()
            .await
            .context("Falha ao buscar no catálogo skills.sh")?
            .error_for_status()
            .context("Erro HTTP na busca do skills.sh")?
            .json()
            .await
            .context("Resposta JSON inválida da busca do skills.sh")?;

        let mut results: Vec<TrendingSkill> = resp
            .skills
            .into_iter()
            .enumerate()
            .map(|(idx, it)| {
                let m = it.installs;
                TrendingSkill {
                    rank: idx + 1,
                    name: it.name,
                    skill_id: it.skill_id,
                    source: it.source,
                    metric_value: m,
                    metric_label: format!("{} installs", format_count(m)),
                    total_installs: m,
                    is_official: it.is_official,
                    trend_sparkline: None,
                }
            })
            .collect();
        results.truncate(limit);
        Ok(results)
    }
}

pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn sparkline(values: &[u64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return String::new();
    }
    let min = *values.iter().min().unwrap_or(&0);
    let max = *values.iter().max().unwrap_or(&0);
    if max == min {
        return BARS[3].to_string().repeat(values.len());
    }
    let span = (max - min) as f64;
    values
        .iter()
        .map(|&v| {
            let norm = ((v - min) as f64 / span * 7.0).round() as usize;
            BARS[norm.min(7)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_count() {
        assert_eq!(format_count(4_421_363), "4.4M");
        assert_eq!(format_count(20_269), "20.3K");
        assert_eq!(format_count(652), "652");
    }

    #[test]
    fn test_sparkline_len_and_bounds() {
        let s = sparkline(&[10, 50, 100]);
        assert_eq!(s.chars().count(), 3);
        assert!(s.starts_with('▁'));
        assert!(s.ends_with('█'));
    }

    #[test]
    fn test_period_parsing() {
        assert_eq!(TrendingPeriod::from_cli_str("dia"), Some(TrendingPeriod::Day));
        assert_eq!(TrendingPeriod::from_cli_str("semana"), Some(TrendingPeriod::Week));
        assert_eq!(TrendingPeriod::from_cli_str("mes"), Some(TrendingPeriod::Month));
        assert_eq!(TrendingPeriod::from_cli_str("mês"), Some(TrendingPeriod::Month));
        assert_eq!(TrendingPeriod::from_cli_str("hot"), Some(TrendingPeriod::Hot));
    }
}
