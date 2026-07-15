mod commands;
mod db;
mod github;
mod gitlab;
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
use commands::mcp::{
    list_mcp_servers, get_mcp_server, create_mcp_server, update_mcp_server, delete_mcp_server,
    list_project_mcp_servers, set_project_mcp_servers, test_mcp_connection,
    export_mcp_server, import_mcp_server,
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
use commands::gitlab_accounts::{
    list_gitlab_accounts, create_gitlab_account, update_gitlab_account,
    delete_gitlab_account, validate_gitlab_account, set_project_gitlab_account,
};
use commands::files::{list_project_files, read_project_file};
use commands::sessions::reconcile_claude_sessions;
use commands::codex_sessions::reconcile_codex_sessions;
use commands::stats::{
    get_usage_stats, backfill_usage, reset_usage_stats, get_budget_status, get_plan_usage,
    refresh_plan_usage,
};
use commands::storage::{get_storage_stats, clean_storage};
use commands::work_digest::get_work_digest;
use commands::runs::{
    start_run, cancel_run, get_run_log, rerun_run, respond_permission,
    send_user_message, end_run_input, read_run_input, resume_run,
    delete_run, delete_all_runs, create_handoff_run, create_session_run,
    rename_run, set_run_pinned,
};
use runs::{new_broker_approvals, new_broker_runs, new_registry, RunRegistry};
use runs::broker::approver::ModalApproverResolver;
use runs::broker::{start_broker, ApproverResolver, BrokerConfig};
use runs::sidecar::kill_process_group;
use std::sync::Arc;

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

            // App-wide singleton credential broker (GĐ7). One Unix socket serves
            // every run for the whole app lifetime, so runs never race a per-run
            // socket that appears/disappears with them (fixes the sporadic
            // "broker unreachable" deny). Token isolation is unchanged: each
            // request is still resolved by its own `project_id`, and an `Ask` is
            // routed to the issuing run's modal (fail-closed if the run ended).
            let approvals = new_broker_approvals();
            let broker_runs = new_broker_runs();
            let resolver: Arc<dyn ApproverResolver> = Arc::new(ModalApproverResolver::new(
                app.handle().clone(),
                approvals.clone(),
                broker_runs.clone(),
            ));
            let broker_handle = tauri::async_runtime::block_on(start_broker(
                db.clone(),
                BrokerConfig { socket_label: "app".to_string(), resolver },
            ))
            .expect("Failed to start credential broker");

            app.manage(db);
            app.manage(new_registry());
            app.manage(approvals);
            app.manage(broker_runs);
            // Keep the broker alive for the whole app lifetime (Drop removes the
            // socket on exit).
            app.manage(broker_handle);
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
            list_mcp_servers,
            get_mcp_server,
            create_mcp_server,
            update_mcp_server,
            delete_mcp_server,
            list_project_mcp_servers,
            set_project_mcp_servers,
            test_mcp_connection,
            export_mcp_server,
            import_mcp_server,
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
            list_gitlab_accounts,
            create_gitlab_account,
            update_gitlab_account,
            delete_gitlab_account,
            validate_gitlab_account,
            set_project_gitlab_account,
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
            get_work_digest,
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
