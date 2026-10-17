//! zen-ext — OWNER: worker D. Settings layering (SPEC §12), ZEN.md memory (§7), skills (§7), hooks (§13).
//! PUBLIC API CONTRACT (zen-cli codes against these exact signatures; keep them, add freely):
//!   load_settings, merge_json, memory_files, memory_prompt, append_memory_note,
//!   discover_skills, skills_prompt, SkillTool, CommandHooks (impl HookRunner)
//! Seed code from the step-2 prototype: ../../.seed-step2/src/settings.rs.

mod commands;
mod hooks;
mod memory;
mod settings;
mod skills;
mod trust;
#[cfg(test)]
mod testutil;

pub use commands::{
    discover_custom_commands, discover_slash_commands, expand_command, expand_slash_prompt,
    find_slash_command, split_args, ExpandContext, SlashCommand, SlashSource,
};
pub use hooks::{matches_target, CommandHookSpec, CommandHooks, OnFailure, HOOK_OUTPUT_LIMIT};
pub use memory::{
    append_memory_note, memory_files, memory_files_with_managed, memory_prompt, MemoryFile,
    MANAGED_MEMORY_PATH, MAX_IMPORT_DEPTH,
};
pub use settings::{
    load_settings, load_settings_trust, load_settings_with_managed, merge_json, LoadedSettings,
};
pub use trust::{
    gated_project_config, is_folder_trusted, project_mcp_files, trust_env_set, trust_folder,
    trust_store_path, TRUST_ENV,
};
pub use skills::{
    discover_skill_entries, discover_skills, load_skill, load_skill_entry, skills_prompt,
    split_frontmatter, Frontmatter, SkillTool,
};
