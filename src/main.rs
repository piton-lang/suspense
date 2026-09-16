// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat_input;
mod checkbox;
mod commit_notes;
mod completion_menu;
mod diff;
mod diff_view;
mod file_link;
mod file_view;
mod folder_browser;
mod fuzzy;
mod git_panel;
mod git_status;
mod harness;
mod harness_mentions;
mod hidden_anchor;
mod inset_panel;
mod main_window;
mod markdown;
mod new_project;
mod palette;
mod piton_build;
mod piton_lsp;
mod piton_syntax;
mod project_directory;
mod project_lsp;
mod project_tree;
mod prompt_history;
mod prompt_mode;
mod prompt_queue;
mod ribbon;
mod scrollbar;
mod settings_window;
mod shell_format;
mod system_prompts;
mod theme_preference;

fn main() {
    app::run();
}
