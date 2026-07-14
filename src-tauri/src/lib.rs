mod commands;
mod db;
mod github;
mod runs;
mod secrets;

use commands::health::health_check;
use commands::settings::{get_settings, update_setting};
use commands::skills::{
    list_skills, get_skill, create_skill, update_skill, delete_skill,
    export_skill_zip, import_skill_zip, open_skill_folder,
};
use commands::rules::{
    list_rules, get_rule, create_rule, update_rule, delete_rule,
    export_rule, import_rule, open_rules_folder,
    get_applied_rules, apply_rule, apply_rule_to_all_projects, remove_rule_from_project,
    list_rule_sync_conflicts, resolve_rule_sync_conflict,
};
use commands::projects::{
    detect_project_info, list_projects, add_project, remove_project, update_project,
    get_applied_skills, apply_skill, apply_skill_to_all_projects, remove_skill_from_project,
    list_sync_conflicts, resolve_sync_conflict,
    list_repos, add_repo, update_repo, remove_repo,
    open_in_vscode, open_in_folder, open_in_terminal,
};
use commands::github::{fetch_issue, fetch_pr, refetch_run, list_runs};
use commands::github_accounts::{
    list_github_accounts, create_github_account, update_github_account,
    delete_github_account, validate_github_account, set_project_github_account,
};
use commands::files::{list_project_files, read_project_file};
use commands::sessions::reconcile_claude_sessions;
use commands::codex_sessions::reconcile_codex_sessions;
use commands::stats::{
    get_usage_stats, backfill_usage, reset_usage_stats, get_budget_status, get_plan_usage,
    refresh_plan_usage,
};
use commands::storage::{get_storage_stats, clean_storage};
use commands::runs::{
    start_run, cancel_run, get_run_log, rerun_run, respond_permission,
    send_user_message, end_run_input, read_run_input, resume_run,
    delete_run, delete_all_runs, create_handoff_run, create_session_run,
    rename_run, set_run_pinned,
};
use runs::{new_registry, RunRegistry};
use runs::sidecar::kill_process_group;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            use tauri::Manager;
            let app_data_dir = app.path().app_data_dir().expect("Failed to get app data dir");
            std::fs::create_dir_all(app_data_dir.join("skills")).expect("Failed to create skills dir");
            std::fs::create_dir_all(app_data_dir.join("rules")).expect("Failed to create rules dir");

            let db_path = app_data_dir.join("data.db");
            let db = tauri::async_runtime::block_on(db::init_db(&db_path))
                .expect("Failed to initialize database");
            // Watch the shared Claude transcript store so sessions created or
            // continued outside Devdy (claude CLI / VS Code) mirror in live.
            runs::session_watcher::start(db.clone(), app.handle().clone());
            app.manage(db);
            app.manage(new_registry());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            health_check,
            get_settings,
            update_setting,
            list_skills,
            get_skill,
            create_skill,
            update_skill,
            delete_skill,
            export_skill_zip,
            import_skill_zip,
            open_skill_folder,
            list_rules,
            get_rule,
            create_rule,
            update_rule,
            delete_rule,
            export_rule,
            import_rule,
            open_rules_folder,
            get_applied_rules,
            apply_rule,
            apply_rule_to_all_projects,
            remove_rule_from_project,
            list_rule_sync_conflicts,
            resolve_rule_sync_conflict,
            detect_project_info,
            list_projects,
            add_project,
            remove_project,
            update_project,
            list_github_accounts,
            create_github_account,
            update_github_account,
            delete_github_account,
            validate_github_account,
            set_project_github_account,
            get_applied_skills,
            apply_skill,
            apply_skill_to_all_projects,
            remove_skill_from_project,
            list_sync_conflicts,
            resolve_sync_conflict,
            list_repos,
            add_repo,
            update_repo,
            remove_repo,
            open_in_vscode,
            open_in_folder,
            open_in_terminal,
            fetch_issue,
            fetch_pr,
            refetch_run,
            list_runs,
            start_run,
            cancel_run,
            get_run_log,
            rerun_run,
            respond_permission,
            send_user_message,
            end_run_input,
            read_run_input,
            resume_run,
            delete_run,
            delete_all_runs,
            rename_run,
            set_run_pinned,
            list_project_files,
            read_project_file,
            create_handoff_run,
            create_session_run,
            get_usage_stats,
            backfill_usage,
            reset_usage_stats,
            get_budget_status,
            get_plan_usage,
            refresh_plan_usage,
            reconcile_claude_sessions,
            reconcile_codex_sessions,
            get_storage_stats,
            clean_storage,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On exit, kill every still-running sidecar (and its CLI child) so
            // no orphaned `claude` / `codex` process lingers burning API tokens.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                use tauri::Manager;
                let registry = app_handle.state::<RunRegistry>().inner().clone();
                tauri::async_runtime::block_on(async move {
                    let mut reg = registry.lock().await;
                    for (_, handles) in reg.iter_mut() {
                        if let Some(pid) = handles.child.id() {
                            kill_process_group(pid);
                        }
                        let _ = handles.child.start_kill();
                    }
                    reg.clear();
                });
            }
        });
}
