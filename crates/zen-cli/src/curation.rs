















use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;




#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,

    #[serde(default = "default_tier")]
    pub trust_tier: String,

    #[serde(default)]
    pub permissions: Vec<String>,

    #[serde(default)]
    pub source: Option<String>,
}

fn default_tier() -> String {
    "community".to_string()
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurationMeta {

    pub reviewer: String,

    pub review_date: String,


    pub review_notes: String,


    #[serde(default)]
    pub reviewed_version: String,
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CuratedEntry {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    pub curation: CurationMeta,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedRegistry {
    pub format_version: u32,
    pub updated: String,
    pub entries: Vec<CuratedEntry>,
}


const EMBEDDED_REGISTRY: &str = include_str!("../registry-curated.json");



pub fn embedded_registry() -> Result<CuratedRegistry> {
    serde_json::from_str(EMBEDDED_REGISTRY).context("embedded registry-curated.json is invalid")
}




pub fn load_curated_registry(home: Option<&Path>) -> Result<CuratedRegistry> {
    let mut reg = embedded_registry()?;
    let user_path = home.map(|h| h.join(".zen").join("registry-curated.json"));
    if let Some(path) = user_path {
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let user_reg: CuratedRegistry = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", path.display()))?;
            if user_reg.format_version != 1 {
                return Err(anyhow!(
                    "unsupported registry-curated.json format_version {} (expected 1)",
                    user_reg.format_version
                ));
            }
            for entry in user_reg.entries {
                let problems = validate_curated_entry(&entry);
                if !problems.is_empty() {
                    return Err(anyhow!(
                        "invalid curated entry '{}' in {}: {}",
                        entry.entry.name,
                        path.display(),
                        problems.join("; ")
                    ));
                }
                if let Some(slot) = reg.entries.iter_mut().find(|e| e.entry.name == entry.entry.name)
                {
                    *slot = entry;
                } else {
                    reg.entries.push(entry);
                }
            }
        }
    }
    Ok(reg)
}



pub fn load_community_registry(home: Option<&Path>) -> Result<Vec<RegistryEntry>> {
    let path = match home {
        Some(h) => h.join(".zen").join("registry.json"),
        None => return Ok(Vec::new()),
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    #[derive(Deserialize)]
    struct CommunityRegistry {
        #[serde(default)]
        entries: Vec<RegistryEntry>,
    }

    if let Ok(reg) = serde_json::from_str::<CommunityRegistry>(&raw) {
        return Ok(reg.entries);
    }
    let entries: Vec<RegistryEntry> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(entries)
}




pub fn validate_curated_entry(entry: &CuratedEntry) -> Vec<String> {
    let mut problems = Vec::new();
    if entry.entry.name.trim().is_empty() {
        problems.push("entry name is empty".to_string());
    }
    if entry.entry.description.trim().is_empty() {
        problems.push(format!("entry '{}': description is empty", entry.entry.name));
    }
    if entry.entry.trust_tier != "curated" {
        problems.push(format!(
            "entry '{}': curated registry entries must have trust_tier \"curated\"",
            entry.entry.name
        ));
    }
    if entry.curation.reviewer.trim().is_empty() {
        problems.push(format!(
            "entry '{}': curation.reviewer is empty",
            entry.entry.name
        ));
    }
    if !valid_date(&entry.curation.review_date) {
        problems.push(format!(
            "entry '{}': curation.review_date '{}' is not a valid YYYY-MM-DD date",
            entry.entry.name, entry.curation.review_date
        ));
    }
    for criterion in ["security", "quality", "documentation", "maintenance"] {
        if !entry
            .curation
            .review_notes
            .to_ascii_lowercase()
            .contains(criterion)
        {
            problems.push(format!(
                "entry '{}': review_notes do not cover '{}'",
                entry.entry.name, criterion
            ));
        }
    }
    if entry.curation.review_notes.trim().len() < 100 {
        problems.push(format!(
            "entry '{}': review_notes too short (<100 chars) to be a real review",
            entry.entry.name
        ));
    }
    problems
}


fn valid_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let (y, m, d) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    );
    let (Ok(y), Ok(m), Ok(d)) = (y, m, d) else {
        return false;
    };
    if parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    if !(1900..=2100).contains(&y) || !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let max_d = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    d <= max_d
}


#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub tier: String,
    pub review_summary: Option<String>,
}




pub fn search_entries(
    query: &str,
    curated_only: bool,
    curated: &CuratedRegistry,
    community: &[RegistryEntry],
) -> Vec<SearchHit> {
    let q = query.to_ascii_lowercase();
    let matches = |e: &RegistryEntry| {
        e.name.to_ascii_lowercase().contains(&q)
            || e.description.to_ascii_lowercase().contains(&q)
            || e.author.to_ascii_lowercase().contains(&q)
    };
    let mut hits: Vec<SearchHit> = Vec::new();
    for ce in &curated.entries {
        if matches(&ce.entry) {
            hits.push(SearchHit {
                name: ce.entry.name.clone(),
                description: ce.entry.description.clone(),
                version: ce.entry.version.clone(),
                author: ce.entry.author.clone(),
                tier: "curated".to_string(),
                review_summary: Some(format!(
                    "reviewed by {} on {} (v{})",
                    ce.curation.reviewer, ce.curation.review_date, ce.curation.reviewed_version
                )),
            });
        }
    }
    if !curated_only {
        for e in community {
            if e.trust_tier == "curated" {
                continue;
            }
            if matches(e) {
                hits.push(SearchHit {
                    name: e.name.clone(),
                    description: e.description.clone(),
                    version: e.version.clone(),
                    author: e.author.clone(),
                    tier: "community".to_string(),
                    review_summary: None,
                });
            }
        }
    }
    hits.sort_by(|a, b| {
        (b.tier == "curated")
            .cmp(&(a.tier == "curated"))
            .then(a.name.cmp(&b.name))
    });
    hits
}


pub fn format_hits(hits: &[SearchHit], curated_only: bool) -> String {
    if hits.is_empty() {
        return if curated_only {
            "No curated skills match. Curated entries are maintainer-reviewed; see docs/plugin-curation.md.\n".to_string()
        } else {
            "No skills match.\n".to_string()
        };
    }
    let mut out = String::new();
    for h in hits {
        let badge = if h.tier == "curated" {
            "[curated ✓]"
        } else {
            "[community]"
        };
        out.push_str(&format!("{} {} v{} - {}\n", badge, h.name, h.version, h.description));
        out.push_str(&format!("    by {}{}\n", h.author, match &h.review_summary {
            Some(s) => format!(" · {}", s),
            None => " · unreviewed - inspect before installing".to_string(),
        }));
    }
    out
}


pub fn run_skill_search(query: &str, curated_only: bool, home: Option<&Path>) -> Result<i32> {
    let curated = load_curated_registry(home)?;

    for entry in &curated.entries {
        let problems = validate_curated_entry(entry);
        if !problems.is_empty() {
            return Err(anyhow!(
                "curated registry entry '{}' failed validation: {}",
                entry.entry.name,
                problems.join("; ")
            ));
        }
    }


    let community = if curated_only {
        Vec::new()
    } else {
        load_community_registry(home)?
    };
    let hits = search_entries(query, curated_only, &curated, &community);
    print!("{}", format_hits(&hits, curated_only));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_curated(name: &str) -> CuratedEntry {
        CuratedEntry {
            entry: RegistryEntry {
                name: name.to_string(),
                description: format!("{} does things", name),
                version: "1.2.0".to_string(),
                author: "ZenCode".to_string(),
                trust_tier: "curated".to_string(),
                permissions: vec!["read".to_string()],
                source: Some(format!("builtin:{}", name)),
            },
            curation: CurationMeta {
                reviewer: "ZC Leader (ZenCode maintainer)".to_string(),
                review_date: "2026-10-17".to_string(),
                review_notes: "Security: PASS. No network. Quality: PASS. Deterministic. Documentation: PASS. Complete. Maintenance: PASS. In-repo.".to_string(),
                reviewed_version: "1.2.0".to_string(),
            },
        }
    }

    fn sample_registry() -> CuratedRegistry {
        CuratedRegistry {
            format_version: 1,
            updated: "2026-10-17".to_string(),
            entries: vec![sample_curated("tdd"), sample_curated("token-diet")],
        }
    }

    fn community_entry(name: &str) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            description: format!("community {}", name),
            version: "0.1.0".to_string(),
            author: "someone".to_string(),
            trust_tier: "community".to_string(),
            permissions: vec![],
            source: Some("https://example.com/skill.zip".to_string()),
        }
    }

    #[test]
    fn embedded_registry_parses_and_validates() {
        let reg = embedded_registry().expect("embedded registry must parse");
        assert_eq!(reg.format_version, 1);
        assert!(!reg.entries.is_empty(), "seed must not be empty");
        for entry in &reg.entries {
            let problems = validate_curated_entry(entry);
            assert!(
                problems.is_empty(),
                "seed entry '{}' invalid: {:?}",
                entry.entry.name,
                problems
            );
        }
    }

    #[test]
    fn seed_contains_three_builtins() {
        let reg = embedded_registry().unwrap();
        let names: Vec<&str> = reg.entries.iter().map(|e| e.entry.name.as_str()).collect();
        for expected in ["tdd", "token-diet", "security-review"] {
            assert!(names.contains(&expected), "missing seed entry {}", expected);
        }
    }

    #[test]
    fn validate_rejects_empty_reviewer() {
        let mut e = sample_curated("x");
        e.curation.reviewer = "   ".to_string();
        assert!(validate_curated_entry(&e)
            .iter()
            .any(|p| p.contains("reviewer")));
    }

    #[test]
    fn validate_rejects_bad_date() {
        for bad in ["2026-13-01", "2026-02-30", "17-10-2026", "2026/10/17", "not-a-date", ""] {
            let mut e = sample_curated("x");
            e.curation.review_date = bad.to_string();
            assert!(
                validate_curated_entry(&e).iter().any(|p| p.contains("review_date")),
                "date '{}' should be rejected",
                bad
            );
        }
    }

    #[test]
    fn validate_accepts_leap_day() {
        let mut e = sample_curated("x");
        e.curation.review_date = "2024-02-29".to_string();
        assert!(validate_curated_entry(&e).is_empty());
    }

    #[test]
    fn validate_requires_all_four_criteria_in_notes() {
        let mut e = sample_curated("x");
        e.curation.review_notes = "Security: PASS. Quality: PASS.".to_string();
        let problems = validate_curated_entry(&e);
        assert!(problems.iter().any(|p| p.contains("documentation")));
        assert!(problems.iter().any(|p| p.contains("maintenance")));
    }

    #[test]
    fn validate_rejects_short_notes() {
        let mut e = sample_curated("x");
        e.curation.review_notes = "Security: PASS. Quality: PASS. Documentation: PASS. Maintenance: PASS.".to_string();
        assert!(validate_curated_entry(&e)
            .iter()
            .any(|p| p.contains("too short")));
    }

    #[test]
    fn validate_rejects_non_curated_tier() {
        let mut e = sample_curated("x");
        e.entry.trust_tier = "community".to_string();
        assert!(validate_curated_entry(&e)
            .iter()
            .any(|p| p.contains("trust_tier")));
    }

    #[test]
    fn search_matches_name_description_author() {
        let reg = sample_registry();
        assert_eq!(search_entries("tdd", false, &reg, &[]).len(), 1);
        assert_eq!(search_entries("does things", false, &reg, &[]).len(), 2);
        assert_eq!(search_entries("zencode", false, &reg, &[]).len(), 2);
        assert_eq!(search_entries("TDD", false, &reg, &[]).len(), 1);
        assert!(search_entries("zzz-no-match", false, &reg, &[]).is_empty());
    }

    #[test]
    fn curated_only_filters_community() {
        let reg = sample_registry();
        let community = vec![community_entry("tdd-helper")];
        let all = search_entries("tdd", false, &reg, &community);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].tier, "curated");
        assert_eq!(all[1].tier, "community");
        let curated = search_entries("tdd", true, &reg, &community);
        assert_eq!(curated.len(), 1);
        assert_eq!(curated[0].tier, "curated");
    }

    #[test]
    fn curated_entries_dedupe_community_shadow() {

        let reg = sample_registry();
        let mut shadow = community_entry("tdd");
        shadow.trust_tier = "curated".to_string();
        let hits = search_entries("tdd", false, &reg, &[shadow]);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn format_shows_badges_and_review_summary() {
        let reg = sample_registry();
        let community = vec![community_entry("tdd-helper")];
        let hits = search_entries("tdd", false, &reg, &community);
        let out = format_hits(&hits, false);
        assert!(out.contains("[curated ✓] tdd"));
        assert!(out.contains("[community] tdd-helper"));
        assert!(out.contains("reviewed by ZC Leader"));
        assert!(out.contains("unreviewed - inspect before installing"));
    }

    #[test]
    fn format_empty_curated_has_helpful_message() {
        let out = format_hits(&[], true);
        assert!(out.contains("No curated skills match"));
        assert!(out.contains("docs/plugin-curation.md"));
    }

    #[test]
    fn valid_date_edge_cases() {
        assert!(valid_date("2026-10-17"));
        assert!(valid_date("2000-02-29"));
        assert!(!valid_date("1900-02-29"));
        assert!(!valid_date("2026-04-31"));
        assert!(!valid_date("2026-1-7"));
        assert!(!valid_date("2026-10-17T00:00:00"));
    }
}
