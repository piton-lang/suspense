// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod activity;
mod agent;
mod animations;
mod answer_blocks;
mod app;
mod attached_image;
mod baked_prompts;
mod chat_input;
mod checkbox;
mod commit_notes;
mod completion_menu;
mod console_text;
mod container;
mod conversations;
mod diff;
mod diff_view;
mod disk_watch;
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
mod hit_areas;
mod inset_panel;
mod login_view;
mod main_window;
mod markdown;
mod measured_list;
mod mode_guard;
mod new_instruction;
mod import_project;
mod new_project;
mod ownership;
mod palette;
mod piton_build;
mod piton_fluency;
mod piton_lsp;
mod piton_syntax;
mod process;
mod process_tree;
mod project;
mod project_directory;
mod project_indicator;
mod project_lsp;
mod project_templates;
mod project_tree;
mod prompt_history;
mod prompt_mode;
mod prompt_queue;
mod prompt_title;
mod raw_prompt;
mod recent_projects;
mod referenced_spec;
mod rescope;
mod rescope_view;
mod ribbon;
mod run_targets;
mod run_view;
mod scrollbar;
mod selection_popover;
mod self_update;
mod settings_window;
mod shell_format;
mod shell_paths;
mod sidebar;
mod spec_component_form;
mod spec_components;
mod subagents;
mod suspense_fluency;
mod system_prompts;
mod task_snapshot;
mod task_table;
#[cfg(all(test, unix))]
mod test_scripts;
mod theme;
mod theme_editor;
mod theme_preference;
mod understanding;
mod usage;
mod version;
mod walkthrough;
mod welcome;

fn main() {
    // `suspense --version` says which build it is, as an edge release's
    // `0.1.N`, and opens nothing.
    if std::env::args().skip(1).any(|arg| arg == "--version") {
        // Its version and the repository it updates from, or that it has
        // none, as the EdgeReleasesScope says. A Windows release opens no
        // console, but a pipe or a file it is redirected to still gets it.
        println!("{}", version::describe());
        return;
    }
    // Run elevated all the same, what it and the programs it starts create
    // is still the user's.
    ownership::own_what_is_created();
    app::run();
}
