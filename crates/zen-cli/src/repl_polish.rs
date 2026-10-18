//! REPL polish commands: `/loop`, `/goal`, `/personality`, `/vim`, `/theme`.
//!
//! Small quality-of-life slash commands deferred from the Claude Code / Codex
//! parity work (see `zencode-vs-claude-codex-parity.md`, "REPL polish").
//! Pure parsing/state helpers live here so they are unit-testable without a
//! running agent; the REPL wires them up in `repl.rs`.

/// Maximum iterations for `/loop` (runaway guard).
pub const MAX_LOOP_ITERATIONS: u32 = 100;

// ─── Theme ───────────────────────────────────────────────────────────────────

/// REPL color theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Dark terminal background (default).
    #[default]
    Dark,
    /// Light terminal background.
    Light,
    /// No ANSI colors at all.
    None,
}

impl Theme {
    /// Parse a theme name (`dark` | `light` | `none`); `None` on unknown input.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            "none" | "off" | "plain" | "no" => Some(Theme::None),
            _ => Option::None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::None => "none",
        }
    }

    /// `true` when the `NO_COLOR` environment variable is present (any value,
    /// even empty, disables colors per https://no-color.org).
    pub fn colors_disabled() -> bool {
        std::env::var_os("NO_COLOR").is_some()
    }

    /// The theme actually used for painting: `NO_COLOR` always wins.
    pub fn effective(self) -> Self {
        if Self::colors_disabled() {
            Theme::None
        } else {
            self
        }
    }

    /// Wrap `text` in ANSI codes for `role`, or return it unchanged when colors
    /// are disabled (theme `none` or `NO_COLOR`).
    pub fn paint(self, role: ColorRole, text: &str) -> String {
        let theme = self.effective();
        if theme == Theme::None {
            return text.to_string();
        }
        let code: u8 = match (theme, role) {
            (Theme::Dark, ColorRole::Prompt) => 36,  // cyan
            (Theme::Dark, ColorRole::Accent) => 33,  // yellow
            (Theme::Dark, ColorRole::Error) => 31,   // red
            (Theme::Dark, ColorRole::Success) => 32, // green
            (Theme::Dark, ColorRole::Dim) => 90,     // bright black
            (Theme::Light, ColorRole::Prompt) => 34, // blue
            (Theme::Light, ColorRole::Accent) => 35, // magenta
            (Theme::Light, ColorRole::Error) => 31,  // red
            (Theme::Light, ColorRole::Success) => 32, // green
            (Theme::Light, ColorRole::Dim) => 90,    // gray
            (Theme::None, _) => unreachable!("handled above"),
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

/// Semantic color roles used by the REPL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRole {
    Prompt,
    Accent,
    Error,
    Success,
    Dim,
}

// ─── Personality ─────────────────────────────────────────────────────────────

/// Response personality: adjusts how verbose the model is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Personality {
    Concise,
    #[default]
    Normal,
    Detailed,
}

impl Personality {
    /// Parse a personality name; `None` on unknown input.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "concise" | "brief" | "terse" | "short" => Some(Personality::Concise),
            "normal" | "default" | "balanced" => Some(Personality::Normal),
            "detailed" | "verbose" | "thorough" | "long" => Some(Personality::Detailed),
            _ => Option::None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Personality::Concise => "concise",
            Personality::Normal => "normal",
            Personality::Detailed => "detailed",
        }
    }

    /// Text appended to the base system prompt. Empty for `Normal` so the
    /// default prompt is byte-identical with no personality set.
    pub fn system_suffix(self) -> &'static str {
        match self {
            Personality::Concise => "\n\n[personality: concise] Be concise: skip preamble and \
                summaries unless asked, give the shortest correct answer, prefer code over prose.",
            Personality::Normal => "",
            Personality::Detailed => "\n\n[personality: detailed] Be thorough: explain your \
                reasoning, mention alternatives you considered, and give detailed answers with \
                examples where they help.",
        }
    }
}

// ─── /loop ───────────────────────────────────────────────────────────────────

/// Parse `/loop <n> <prompt>` args into `(iterations, prompt)`.
/// Returns a human-readable error string on invalid input.
pub fn parse_loop_args(arg: &str) -> Result<(u32, String), String> {
    let mut it = arg.splitn(2, char::is_whitespace);
    let n_str = it.next().unwrap_or("").trim();
    let prompt = it.next().unwrap_or("").trim();
    if n_str.is_empty() || prompt.is_empty() {
        return Err("usage: /loop <n> <prompt>".to_string());
    }
    let n: u32 = n_str
        .parse()
        .map_err(|_| format!("invalid iteration count {n_str:?}"))?;
    if n == 0 {
        return Err("iteration count must be at least 1".to_string());
    }
    if n > MAX_LOOP_ITERATIONS {
        return Err(format!(
            "iteration count capped at {MAX_LOOP_ITERATIONS} (got {n})"
        ));
    }
    Ok((n, prompt.to_string()))
}

// ─── Prompt line ─────────────────────────────────────────────────────────────

/// Maximum goal characters shown in the prompt line.
pub const GOAL_PREVIEW_LEN: usize = 48;

/// Render the REPL input prompt. When a session goal is set it is shown on its
/// own dimmed line above the `> ` prompt (the closest thing to a status line in
/// the plain-stdin REPL). The `> ` itself is themed.
pub fn prompt_text(theme: Theme, goal: &Option<String>) -> String {
    let mut out = String::new();
    if let Some(g) = goal {
        let shown: String = g.chars().take(GOAL_PREVIEW_LEN).collect();
        let ellipsis = if g.chars().count() > GOAL_PREVIEW_LEN {
            "…"
        } else {
            ""
        };
        out.push_str(&theme.paint(ColorRole::Dim, &format!("[goal: {shown}{ellipsis}]")));
        out.push('\n');
    }
    out.push_str(&theme.paint(ColorRole::Prompt, "> "));
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_args_valid() {
        let (n, p) = parse_loop_args("3 run the tests").unwrap();
        assert_eq!(n, 3);
        assert_eq!(p, "run the tests");
    }

    #[test]
    fn loop_args_missing_count() {
        assert!(parse_loop_args("").is_err());
        assert!(parse_loop_args("run the tests").is_err());
    }

    #[test]
    fn loop_args_missing_prompt() {
        assert_eq!(
            parse_loop_args("3").unwrap_err(),
            "usage: /loop <n> <prompt>"
        );
    }

    #[test]
    fn loop_args_zero_rejected() {
        assert!(parse_loop_args("0 do x").unwrap_err().contains("at least 1"));
    }

    #[test]
    fn loop_args_over_cap_rejected() {
        let err = parse_loop_args("101 do x").unwrap_err();
        assert!(err.contains("capped at 100"), "got: {err}");
    }

    #[test]
    fn loop_args_at_cap_ok() {
        let (n, _) = parse_loop_args("100 do x").unwrap();
        assert_eq!(n, 100);
    }

    #[test]
    fn loop_args_non_numeric_rejected() {
        assert!(parse_loop_args("many do x").is_err());
    }

    #[test]
    fn theme_parse_names() {
        assert_eq!(Theme::parse("dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("Light"), Some(Theme::Light));
        assert_eq!(Theme::parse("NONE"), Some(Theme::None));
        assert_eq!(Theme::parse("off"), Some(Theme::None));
        assert_eq!(Theme::parse("solarized"), Option::None);
        assert_eq!(Theme::parse(""), Option::None);
    }

    #[test]
    fn theme_none_paint_is_plain() {
        assert_eq!(Theme::None.paint(ColorRole::Prompt, "> "), "> ");
        assert_eq!(Theme::None.paint(ColorRole::Error, "boom"), "boom");
    }

    #[test]
    fn theme_dark_paint_adds_ansi() {
        let painted = Theme::Dark.paint(ColorRole::Prompt, "> ");
        assert!(painted.contains("\x1b["), "got: {painted:?}");
        assert!(painted.contains("> "));
        assert!(painted.ends_with("\x1b[0m"));
    }

    #[test]
    fn theme_light_differs_from_dark() {
        let dark = Theme::Dark.paint(ColorRole::Prompt, "x");
        let light = Theme::Light.paint(ColorRole::Prompt, "x");
        assert_ne!(dark, light);
    }

    #[test]
    fn personality_parse_names() {
        assert_eq!(Personality::parse("concise"), Some(Personality::Concise));
        assert_eq!(Personality::parse("brief"), Some(Personality::Concise));
        assert_eq!(Personality::parse("normal"), Some(Personality::Normal));
        assert_eq!(Personality::parse("default"), Some(Personality::Normal));
        assert_eq!(Personality::parse("detailed"), Some(Personality::Detailed));
        assert_eq!(Personality::parse("verbose"), Some(Personality::Detailed));
        assert_eq!(Personality::parse("pirate"), Option::None);
        assert_eq!(Personality::parse(""), Option::None);
    }

    #[test]
    fn personality_normal_suffix_empty() {
        assert_eq!(Personality::Normal.system_suffix(), "");
    }

    #[test]
    fn personality_suffixes_distinct_and_marked() {
        let c = Personality::Concise.system_suffix();
        let d = Personality::Detailed.system_suffix();
        assert_ne!(c, d);
        assert!(c.contains("[personality: concise]"), "got: {c}");
        assert!(d.contains("[personality: detailed]"), "got: {d}");
    }

    #[test]
    fn prompt_without_goal_is_plain_prompt() {
        // Theme::None keeps the assertion independent of terminal colors.
        assert_eq!(prompt_text(Theme::None, &Option::None), "> ");
    }

    #[test]
    fn prompt_with_goal_shows_goal_line() {
        let out = prompt_text(Theme::None, &Some("ship v1".to_string()));
        assert!(out.contains("[goal: ship v1]"), "got: {out:?}");
        assert!(out.ends_with("> "));
    }

    #[test]
    fn prompt_truncates_long_goal() {
        let long = "x".repeat(100);
        let out = prompt_text(Theme::None, &Some(long));
        assert!(out.contains("…"), "got: {out:?}");
        assert!(!out.contains(&"x".repeat(100)));
    }
}
