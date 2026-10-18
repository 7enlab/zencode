//! Community skill registry: the discoverability layer for ZenCode skills.
//!
//! The skills system ([`crate::skills`]) handles install trust; this module
//! handles *finding* skills:
//!
//! - A curated registry ships with the binary
//!   (`skills/community-registry.json`, embedded at compile time). It lists the
//!   built-in skills plus reviewed community examples.
//! - `zen skill search <query>` ranks entries by name / tag / description match.
//! - `zen skill info <name>` shows full details (trust tier, permissions) so the
//!   user can decide BEFORE installing.
//! - `zen skill install registry:<name> [--yes]` resolves the entry's source
//!   and reuses the standard install flow — permission diff, explicit `--yes`,
//!   hash pinning — unchanged.
//! - `--registry <path>` overrides the bundled registry with a local file, the
//!   stepping stone to a future HTTP registry (see `docs/skill-registry.md`).
//!
//! Security: the registry is *advisory*. The tier shown comes from the registry
//! entry, but the tier actually assigned at install is computed by
//! [`crate::skills::assign_tier`] from the fetched manifest and its signature.
//! A registry claiming "verified" for an unsigned skill installs as community
//! or unverified — the registry cannot upgrade trust.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::skills::{SkillPermissions, TrustTier};

/// Current registry schema version. Bump on incompatible schema changes.
pub const REGISTRY_FORMAT_VERSION: u32 = 1;

/// Where a registry skill can be installed from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RegistrySource {
    /// Ships with the binary; installs via `builtin:<name>`.
    Builtin,
    /// Clone from a git URL; installs via the standard git flow.
    Git { url: String },
}

impl RegistrySource {
    pub fn describe(&self) -> String {
        match self {
            RegistrySource::Builtin => "builtin".to_string(),
            RegistrySource::Git { url } => format!("git:{url}"),
        }
    }
}

/// One skill listed in the community registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySkill {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    /// Advisory trust tier — the install re-computes the real tier (see module docs).
    pub tier: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub permissions: SkillPermissions,
    pub source: RegistrySource,
}

impl RegistrySkill {
    /// Validate field shapes. Returns a list of problems (empty = valid).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = vec![];
        if self.name.trim().is_empty() {
            problems.push("name is empty".to_string());
        } else if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            problems.push(format!("name {:?} must be [a-zA-Z0-9_-]+", self.name));
        }
        if self.version.trim().is_empty() {
            problems.push(format!("skill {:?}: version is empty", self.name));
        }
        if self.description.trim().is_empty() {
            problems.push(format!("skill {:?}: description is empty", self.name));
        }
        if TrustTier::parse(&self.tier).is_none() {
            problems.push(format!(
                "skill {:?}: tier {:?} must be one of: verified, community, unverified",
                self.name, self.tier
            ));
        }
        if !["project", "user"].contains(&self.permissions.file_scope.as_str()) {
            problems.push(format!(
                "skill {:?}: permissions.file_scope {:?} must be \"project\" or \"user\"",
                self.name, self.permissions.file_scope
            ));
        }
        if let RegistrySource::Git { url } = &self.source {
            if !(url.starts_with("https://") || url.starts_with("git@")) {
                problems.push(format!(
                    "skill {:?}: git url {:?} must start with https:// or git@",
                    self.name, url
                ));
            }
        }
        problems
    }

    pub fn tier_enum(&self) -> TrustTier {
        TrustTier::parse(&self.tier).unwrap_or(TrustTier::Unverified)
    }
}

/// The community registry document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityRegistry {
    pub format_version: u32,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub skills: Vec<RegistrySkill>,
}

/// The curated registry shipped with the binary.
const BUNDLED_REGISTRY_JSON: &str = include_str!("../../../skills/community-registry.json");

/// Load the community registry.
///
/// - `None` → the curated registry embedded in the binary.
/// - `Some(path)` → a local registry file in the same schema. A future HTTP
///   registry uses this schema too; download it and pass `--registry <path>`
///   until then.
pub fn load_community_registry(override_path: Option<&str>) -> Result<CommunityRegistry> {
    match override_path {
        None => parse_community_registry(BUNDLED_REGISTRY_JSON),
        Some(p) if p.starts_with("https://") || p.starts_with("http://") => Err(anyhow!(
            "HTTP registries are not supported yet (see docs/skill-registry.md): \
             download the registry JSON and pass --registry <local-path>"
        )),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading registry file {p:?}"))?;
            parse_community_registry(&text)
        }
    }
}

fn parse_community_registry(text: &str) -> Result<CommunityRegistry> {
    let reg: CommunityRegistry =
        serde_json::from_str(text).context("parsing community registry JSON")?;
    if reg.format_version != REGISTRY_FORMAT_VERSION {
        return Err(anyhow!(
            "unsupported registry format_version {} (this zen supports {})",
            reg.format_version,
            REGISTRY_FORMAT_VERSION
        ));
    }
    let mut problems = vec![];
    let mut seen = std::collections::HashSet::new();
    for s in &reg.skills {
        if !seen.insert(s.name.to_ascii_lowercase()) {
            problems.push(format!("duplicate skill name {:?}", s.name));
        }
        problems.extend(s.validate());
    }
    if !problems.is_empty() {
        return Err(anyhow!(
            "invalid community registry:\n  - {}",
            problems.join("\n  - ")
        ));
    }
    Ok(reg)
}

/// Find a skill by name (case-insensitive).
pub fn find_in_registry<'a>(reg: &'a CommunityRegistry, name: &str) -> Option<&'a RegistrySkill> {
    reg.skills
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Ranked search over name, tags, description.
/// Score: exact name = 100, name prefix = 75, name substring = 50,
/// tag exact = 40, description substring = 20. Sorted by score desc, name asc.
pub fn search_registry<'a>(
    reg: &'a CommunityRegistry,
    query: &str,
) -> Vec<(u32, &'a RegistrySkill)> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut hits: Vec<(u32, &'a RegistrySkill)> = vec![];
    for s in &reg.skills {
        let name = s.name.to_ascii_lowercase();
        let desc = s.description.to_ascii_lowercase();
        let mut score = 0u32;
        if name == q {
            score = score.max(100);
        } else if name.starts_with(&q) {
            score = score.max(75);
        } else if name.contains(&q) {
            score = score.max(50);
        }
        if s.tags.iter().any(|t| t.to_ascii_lowercase() == q) {
            score = score.max(40);
        }
        if desc.contains(&q) {
            score = score.max(20);
        }
        if score > 0 {
            hits.push((score, s));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    hits
}

/// Render `zen skill info` output: full details + install guidance.
pub fn format_skill_info(s: &RegistrySkill) -> String {
    let tier = s.tier_enum();
    let mut out = String::new();
    out.push_str(&format!("{} {} v{}\n", tier.badge(), s.name, s.version));
    out.push_str(&format!("  {}\n", s.description));
    if !s.author.is_empty() {
        out.push_str(&format!("  author: {}\n", s.author));
    }
    if !s.tags.is_empty() {
        out.push_str(&format!("  tags: {}\n", s.tags.join(", ")));
    }
    out.push_str(&format!(
        "  trust tier (registry): {} {}\n",
        tier.badge(),
        tier.as_str()
    ));
    out.push_str("  permissions:\n");
    for line in s.permissions.describe() {
        out.push_str(&format!("    {line}\n"));
    }
    out.push_str(&format!("  source: {}\n", s.source.describe()));
    out.push_str(
        "\n  NOTE: install re-verifies trust from the fetched manifest;\n\
         \x20 a registry \"verified\" claim without a valid ZenCode signature\n\
         \x20 installs as community/unverified instead.\n",
    );
    out.push_str(&format!(
        "\n  install: zen skill install registry:{} --yes\n",
        s.name
    ));
    out
}

/// `zen skill search` implementation.
pub fn cmd_search(query: &str, registry_override: Option<&str>) -> Result<()> {
    let reg = load_community_registry(registry_override)?;
    let hits = search_registry(&reg, query);
    if hits.is_empty() {
        println!("no skills found for {query:?} in the community registry.");
        return Ok(());
    }
    println!("registry results for {query:?}:");
    for (_, s) in &hits {
        println!(
            "  {} {} v{} — {}",
            s.tier_enum().badge(),
            s.name,
            s.version,
            s.description
        );
    }
    println!("\ndetails: zen skill info <name>   |   install: zen skill install registry:<name> --yes");
    Ok(())
}

/// `zen skill info` implementation.
pub fn cmd_info(name: &str, registry_override: Option<&str>) -> Result<()> {
    let reg = load_community_registry(registry_override)?;
    match find_in_registry(&reg, name) {
        Some(s) => {
            println!("{}", format_skill_info(s));
            Ok(())
        }
        None => Err(anyhow!(
            "no skill {name:?} in the community registry (try `zen skill search {name}`)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> CommunityRegistry {
        load_community_registry(None).expect("bundled registry must parse")
    }

    #[test]
    fn bundled_registry_parses_and_validates() {
        let reg = seed();
        assert_eq!(reg.format_version, REGISTRY_FORMAT_VERSION);
        // 3 builtins + 3 community examples.
        assert_eq!(reg.skills.len(), 6);
        for name in ["tdd", "token-diet", "security-review"] {
            let s = find_in_registry(&reg, name).unwrap();
            assert_eq!(s.tier_enum(), TrustTier::Verified);
            assert!(matches!(s.source, RegistrySource::Builtin));
        }
    }

    #[test]
    fn bundled_registry_matches_builtin_manifests() {
        // The registry's permission declarations must match the manifests that
        // install_builtin() embeds — drift here misleads `zen skill info`.
        let reg = seed();
        let tdd = find_in_registry(&reg, "tdd").unwrap();
        assert!(tdd.permissions.exec);
        assert!(tdd.permissions.file_write);
        assert!(!tdd.permissions.network);
        let diet = find_in_registry(&reg, "token-diet").unwrap();
        assert!(!diet.permissions.exec);
        assert!(!diet.permissions.file_write);
        assert_eq!(diet.permissions.allowed_tools, vec!["read".to_string()]);
    }

    #[test]
    fn search_exact_name_ranks_first() {
        let reg = seed();
        let hits = search_registry(&reg, "tdd");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].1.name, "tdd");
        assert_eq!(hits[0].0, 100);
    }

    #[test]
    fn search_prefix_beats_description() {
        let reg = seed();
        // "sec" is a name prefix of security-review (75) and also appears in
        // descriptions; prefix must win.
        let hits = search_registry(&reg, "sec");
        assert_eq!(hits[0].1.name, "security-review");
        assert_eq!(hits[0].0, 75);
    }

    #[test]
    fn search_tag_match() {
        let reg = seed();
        let hits = search_registry(&reg, "testing");
        assert!(hits.iter().any(|(_, s)| s.name == "tdd"));
        // tag exact match scores 40.
        let tdd = hits.iter().find(|(_, s)| s.name == "tdd").unwrap();
        assert_eq!(tdd.0, 40);
    }

    #[test]
    fn search_description_match_scores_lowest() {
        let reg = seed();
        // "unfamiliar" appears only in explain-code's description.
        let hits = search_registry(&reg, "unfamiliar");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.name, "explain-code");
        assert_eq!(hits[0].0, 20);
    }

    #[test]
    fn search_no_match_is_empty() {
        let reg = seed();
        assert!(search_registry(&reg, "zzz-no-such-skill").is_empty());
        assert!(search_registry(&reg, "   ").is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let reg = seed();
        let hits = search_registry(&reg, "TDD");
        assert_eq!(hits[0].1.name, "tdd");
    }

    #[test]
    fn find_is_case_insensitive() {
        let reg = seed();
        assert!(find_in_registry(&reg, "Token-Diet").is_some());
        assert!(find_in_registry(&reg, "nope").is_none());
    }

    #[test]
    fn registry_schema_rejects_bad_entries() {
        let bad = r#"{
            "format_version": 1,
            "skills": [
                {"name": "bad name!", "description": "x", "version": "1",
                 "tier": "verified", "source": {"kind": "builtin"}},
                {"name": "badtier", "description": "x", "version": "1",
                 "tier": "gold", "source": {"kind": "builtin"}},
                {"name": "badurl", "description": "x", "version": "1",
                 "tier": "community", "source": {"kind": "git", "url": "ftp://x/y"}},
                {"name": "badscope", "description": "x", "version": "1",
                 "tier": "community",
                 "permissions": {"file_scope": "everywhere"},
                 "source": {"kind": "git", "url": "https://example.com/r"}}
            ]
        }"#;
        let err = parse_community_registry(bad).unwrap_err().to_string();
        assert!(err.contains("bad name!"), "{err}");
        assert!(err.contains("\"gold\""), "{err}");
        assert!(err.contains("ftp://x/y"), "{err}");
        assert!(err.contains("everywhere"), "{err}");
    }

    #[test]
    fn registry_rejects_duplicate_names() {
        let dup = r#"{
            "format_version": 1,
            "skills": [
                {"name": "tdd", "description": "x", "version": "1",
                 "tier": "verified", "source": {"kind": "builtin"}},
                {"name": "TDD", "description": "y", "version": "2",
                 "tier": "verified", "source": {"kind": "builtin"}}
            ]
        }"#;
        let err = parse_community_registry(dup).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn registry_rejects_future_format_version() {
        let future = r#"{"format_version": 99, "skills": []}"#;
        let err = parse_community_registry(future).unwrap_err().to_string();
        assert!(err.contains("format_version"), "{err}");
    }

    #[test]
    fn http_registry_override_is_rejected_with_guidance() {
        let err = load_community_registry(Some("https://example.com/registry.json"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not supported yet"), "{err}");
    }

    #[test]
    fn local_registry_override_loads() {
        let dir = std::env::temp_dir().join(format!("zen-reg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom.json");
        std::fs::write(
            &path,
            r#"{"format_version": 1, "skills": [
                {"name": "mine", "description": "my skill", "version": "0.1",
                 "tier": "unverified", "source": {"kind": "git", "url": "https://example.com/r"}}
            ]}"#,
        )
        .unwrap();
        let reg = load_community_registry(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(reg.skills.len(), 1);
        assert_eq!(reg.skills[0].name, "mine");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn info_format_shows_tier_permissions_and_install() {
        let reg = seed();
        let s = find_in_registry(&reg, "security-review").unwrap();
        let text = format_skill_info(s);
        assert!(text.contains("[✓ verified]"), "{text}");
        assert!(text.contains("security-review"), "{text}");
        assert!(text.contains("network: no"), "{text}");
        assert!(text.contains("zen skill install registry:security-review --yes"), "{text}");
        // The trust caveat must be present.
        assert!(text.contains("re-verifies trust"), "{text}");
    }

    #[test]
    fn registry_git_source_describes_url() {
        let reg = seed();
        let s = find_in_registry(&reg, "commit-msg").unwrap();
        match &s.source {
            RegistrySource::Git { url } => {
                assert!(url.starts_with("https://"), "{url}");
                assert_eq!(s.tier_enum(), TrustTier::Community);
            }
            other => panic!("expected git source, got {other:?}"),
        }
    }
}
