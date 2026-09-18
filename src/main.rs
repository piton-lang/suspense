// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity;
mod animations;
mod app;
mod baked_prompts;
mod chat_input;
mod checkbox;
mod commit_notes;
mod completion_menu;
mod conversations;
mod diff;
mod diff_view;
mod divergence;
mod divergence_view;
#[cfg(test)]
mod double_borders;
mod file_link;
mod file_view;
#[cfg(test)]
mod frame_image;
mod fs_browser;
mod fuzzy;
mod generate_skills;
mod generate_skills_view;
mod git_panel;
mod git_status;
mod growing_input;
mod harness;
mod harness_mentions;
mod hidden_anchor;
mod inset_panel;
mod main_window;
mod markdown;
mod measured_list;
mod new_instruction;
mod new_project;
mod palette;
mod piton_build;
mod piton_fluency;
mod piton_lsp;
mod piton_syntax;
mod project;
mod project_directory;
mod project_indicator;
mod project_lsp;
mod project_templates;
mod project_tree;
mod prompt_history;
mod prompt_mode;
mod prompt_queue;
mod recent_projects;
mod referenced_spec;
mod rescope;
mod rescope_view;
mod ribbon;
mod scrollbar;
mod selection_popover;
mod settings_window;
mod shell_format;
mod shell_paths;
mod sidebar;
mod spec_component_form;
mod spec_components;
mod system_prompts;
mod task_table;
mod theme;
mod theme_preference;
mod understanding;

fn main() {
    app::run();
}
