





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
    memories_report, detect_import_candidates,
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
