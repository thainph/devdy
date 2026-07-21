mod commands;
mod db;
mod github;
mod gitlab;
mod runs;
mod secrets;

use commands::aws_accounts::{
    create_aws_account, delete_aws_account, list_aws_accounts, set_project_aws_account,
    update_aws_account, validate_aws_account,
};
use commands::codex_sessions::reconcile_codex_sessions;
use commands::files::{list_project_files, read_file_base64, read_project_file};
use commands::github::{fetch_issue, fetch_pr, list_runs, refetch_run};
use commands::github_accounts::{
    create_github_account, delete_github_account, list_github_accounts, set_project_github_account,
    update_github_account, validate_github_account,
};
use commands::gitlab_accounts::{
    create_gitlab_account, delete_gitlab_account, list_gitlab_accounts, set_project_gitlab_account,
    update_gitlab_account, validate_gitlab_account,
};
use commands::health::health_check;
use commands::mcp::{
    create_mcp_server, delete_mcp_server, export_mcp_server, get_mcp_server, import_mcp_server,
    list_mcp_servers, list_project_mcp_servers, set_project_mcp_servers, test_mcp_connection,
    update_mcp_server,
};
use commands::notifications::show_permission_notification;
use commands::projects::{
    add_project, add_repo, apply_skill, apply_skill_to_all_projects, detect_project_info,
    get_applied_skills, list_projects, list_repos, list_sync_conflicts, open_in_folder,
    open_in_terminal, open_in_vscode, remove_project, remove_repo, remove_skill_from_project,
    resolve_sync_conflict, update_project, update_repo,
};
use commands::rules::{
    apply_rule, apply_rule_to_all_projects, create_rule, delete_rule, export_rule,
    get_applied_rules, get_rule, import_rule, list_rule_sync_conflicts, list_rules,
    open_rules_folder, remove_rule_from_project, resolve_rule_sync_conflict, update_rule,
};
use commands::runs::{
    cancel_run, create_handoff_run, create_session_run, delete_all_runs, delete_run, end_run_input,
    get_run_log, get_run_log_path, read_run_input, rename_run, rerun_run, respond_permission,
    resume_run, send_user_message, set_run_pinned, start_run,
};
use commands::sessions::reconcile_claude_sessions;
use commands::settings::{get_settings, update_setting};
use commands::skills::{
    create_skill, delete_skill, export_skill_zip, get_skill, import_skill_zip, list_skills,
    open_skill_folder, update_skill,
};
use commands::stats::{
    backfill_usage, get_budget_status, get_codex_budget_status, get_plan_usage,
    get_plan_usage_codex, get_usage_stats, refresh_codex_plan_usage, refresh_plan_usage,
    reset_usage_stats,
};
use commands::storage::{clean_storage, get_storage_stats};
use commands::vps_servers::{
    create_vps_server, delete_vps_server, list_project_servers, list_vps_servers,
    map_server_to_project, test_vps_connection, unmap_server, update_vps_server,
};
use commands::work_digest::get_work_digest;
use commands::work_summary::{cancel_work_summary, summarize_work_digest, WorkSummaryState};
use runs::broker::approver::ModalApproverResolver;
use runs::broker::{start_broker, ApproverResolver, BrokerConfig};
use runs::sidecar::kill_process_group;
use runs::{new_broker_approvals, new_broker_runs, new_registry, RunRegistry};
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
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir");
            std::fs::create_dir_all(app_data_dir.join("skills"))
                .expect("Failed to create skills dir");
            std::fs::create_dir_all(app_data_dir.join("rules"))
                .expect("Failed to create rules dir");

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
                BrokerConfig {
                    socket_label: "app".to_string(),
                    resolver,
                },
            ))
            .expect("Failed to start credential broker");

            app.manage(db);
            app.manage(WorkSummaryState::default());
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
            list_vps_servers,
            create_vps_server,
            update_vps_server,
            delete_vps_server,
            test_vps_connection,
            list_project_servers,
            map_server_to_project,
            unmap_server,
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
            list_aws_accounts,
            create_aws_account,
            update_aws_account,
            delete_aws_account,
            validate_aws_account,
            set_project_aws_account,
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
            get_run_log_path,
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
            read_file_base64,
            create_handoff_run,
            create_session_run,
            get_usage_stats,
            get_work_digest,
            summarize_work_digest,
            cancel_work_summary,
            backfill_usage,
            reset_usage_stats,
            get_budget_status,
            get_codex_budget_status,
            get_plan_usage,
            get_plan_usage_codex,
            refresh_plan_usage,
            refresh_codex_plan_usage,
            reconcile_claude_sessions,
            reconcile_codex_sessions,
            get_storage_stats,
            clean_storage,
            show_permission_notification,
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
