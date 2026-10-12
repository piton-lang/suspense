// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod debug_log;
mod activity;
mod agent;
mod animations;
mod answer_blocks;
mod app;
mod attached_file;
mod attached_image;
mod baked_prompts;
mod chat_input;
mod checkbox;
mod commit_notes;
mod completion_menu;
mod console_text;
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
mod harness_login;
mod harness_mentions;
mod hidden_anchor;
mod hit_areas;
mod inset_panel;
mod main_window;
mod markdown;
mod measured_list;
mod effort;
mod mode_guard;
mod models;
mod new_instruction;
mod import_project;
mod new_project;
mod ownership;
mod output_rate;
mod time_breakdown;
mod palette;
mod piton_build;
mod piton_fluency;
mod piton_lsp;
mod piton_syntax;
mod piton_wrap;
mod process;
mod programs;
mod process_tree;
mod project;
mod project_directory;
mod project_settings;
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
mod sandbox;
mod ribbon;
mod run_targets;
mod run_view;
mod scrollbar;
mod selection_popover;
mod self_update;
mod session_keep;
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
    // Run as a sandboxed run's `piton`, it asks the application to run
    // piton as trusted work, and opens nothing.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(sandbox::channel::SHIM_ARG) {
        std::process::exit(sandbox::channel::shim(args[1..].to_vec()));
    }
    // Run as a sandboxed run's launcher on Windows, it makes the run's
    // AppContainer and grants, runs the harness in it, and takes them away.
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some(sandbox::windows::LAUNCH_ARG) {
        std::process::exit(sandbox::windows::launch(&args[1..]));
    }
    // Whatever a run the application didn't see end left granted, as after
    // a crash, is taken away before anything else.
    #[cfg(windows)]
    sandbox::windows::cleanup_left();
    // `suspense --sandbox <profile> <project> -- <command>...` runs one
    // command under a profile, as the SandboxScope's examples do.
    if args.first().map(String::as_str) == Some("--sandbox") {
        std::process::exit(sandbox::example(&args[1..]));
    }
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
    // Programs are found where the user's terminal finds them.
    programs::read_at_launch();
    app::run();
}
