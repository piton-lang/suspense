// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat_input;
mod completion_menu;
mod file_link;
mod file_view;
mod fuzzy;
mod harness;
mod harness_mentions;
mod hidden_anchor;
mod main_window;
mod markdown;
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
mod settings_window;
mod shell_format;
mod system_prompts;
mod theme_preference;
mod toolbar;

fn main() {
    app::run();
}
