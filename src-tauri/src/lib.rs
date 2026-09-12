mod commands;
mod db;
mod github;
mod gitlab;
mod remote;
mod runs;
mod secrets;

use commands::aws_accounts::{
    create_aws_account, delete_aws_account, list_aws_accounts, set_project_aws_account,
    update_aws_account, validate_aws_account,
};
use commands::claude_accounts::{
    create_claude_account, delete_claude_account, list_claude_accounts,
    open_claude_account_login, rename_claude_account, set_default_claude_account,
    set_project_claude_account, validate_claude_account,
};
use commands::codex_sessions::reconcile_codex_sessions;
use commands::file_tree_watcher::{
    set_file_tree_watch, stop_file_tree_watch, FileTreeWatchers,
};
use commands::files::{
    copy_entry, create_dir, create_file, delete_entry, list_dir, list_project_files, move_entry,
    read_file_base64, read_project_file, rename_entry, write_project_file,
};
use commands::github::{
    fetch_issue, fetch_pr, list_milestone_board, list_milestone_issues,
    list_review_requested_prs, list_runs, refetch_run, resolve_project_board,
};
use commands::github_accounts::{
    create_github_account, delete_github_account, list_github_accounts, set_project_github_account,
    update_github_account, validate_github_account,
};
use commands::gitlab_accounts::{
    create_gitlab_account, delete_gitlab_account, list_gitlab_accounts, set_project_gitlab_account,
    update_gitlab_account, validate_gitlab_account,
};
use commands::health::health_check;
use commands::mascot_speak::{cancel_mascot_speak, mascot_speak, MascotSpeakState};
use commands::mcp::{
    create_mcp_server, delete_mcp_server, export_mcp_server, get_mcp_server, import_mcp_server,
    list_mcp_servers, list_project_mcp_servers, set_project_mcp_servers, test_mcp_connection,
    update_mcp_server,
};
use commands::gcalendar::{
    create_google_calendar_event, delete_google_calendar_event, list_google_calendar_events,
    list_google_calendars, update_google_calendar_event,
};
use commands::google::{
    add_google_account, delete_google_account, google_client_status, google_forget_client,
    list_google_accounts, rename_google_account, set_default_google_account,
};
use commands::notifications::{show_calendar_reminder, show_permission_notification};
use commands::projects::{
    add_project, add_repo, apply_skill, apply_skill_to_all_projects, detect_project_info,
    get_applied_skills, list_projects, list_repos, list_sync_conflicts, open_in_chrome,
    open_in_folder, open_in_terminal, open_in_vscode, remove_project, remove_repo,
    remove_skill_from_project, reorder_projects,
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
    resume_run, send_user_message, set_run_claude_account, set_run_pinned, start_run,
};
use commands::sessions::reconcile_claude_sessions;
use commands::settings::{get_settings, update_setting};
use commands::skills::{
    create_skill, delete_skill, export_skill_zip, get_skill, import_skill_zip, list_skills,
    open_skill_folder, update_skill,
};
use commands::stats::{
    backfill_usage, get_budget_status, get_claude_account_budget, get_codex_budget_status,
    get_plan_usage, get_plan_usage_codex, get_run_budget, get_usage_stats,
    refresh_codex_plan_usage, refresh_plan_usage, reset_usage_stats,
};
use commands::storage::{clean_storage, get_storage_stats};
use commands::notes::{
    add_note, delete_note, delete_notes, list_notes, reorder_notes, set_note_project, update_note,
};
use commands::todos::{
    add_todo, clear_completed_todos, delete_todo, delete_todos, list_todos, reorder_todos,
    set_todo_project, toggle_todo, update_todo,
};
use commands::vps_servers::{
    create_vps_server, delete_vps_server, list_project_servers, list_vps_servers,
    map_server_to_project, test_vps_connection, unmap_server, update_vps_server,
};
use commands::work_digest::get_work_digest;
use commands::models::{
    get_model_caches, list_claude_models, refresh_claude_models, refresh_codex_models,
};
use commands::translate::{cancel_translate, prewarm_translate, translate_text, TranslateState};
use commands::work_summary::{cancel_work_summary, summarize_work_digest, WorkSummaryState};
use remote::commands::{
    remote_clear_audit, remote_create_session_link, remote_disable, remote_enable,
    remote_end_session, remote_get_audit, remote_reveal_otp, remote_set_config,
    remote_set_master_password, remote_status,
};
use remote::RemoteState;
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
                    // Tách socket theo build để bản dev và bản đã cài không
                    // giành chung /tmp/devdy-broker/app.sock (start_broker xoá
                    // socket cũ khi khởi động; nếu trùng label sẽ kill broker
                    // của bản kia khi chạy song song).
                    socket_label: if cfg!(debug_assertions) { "app-dev" } else { "app" }.to_string(),
                    resolver,
                },
            ))
            .expect("Failed to start credential broker");

            app.manage(db);
            app.manage(FileTreeWatchers::default());
            app.manage(WorkSummaryState::default());
            app.manage(TranslateState::default());
            app.manage(MascotSpeakState::default());
            app.manage(new_registry());
            app.manage(approvals);
            app.manage(broker_runs);
            // Keep the broker alive for the whole app lifetime (Drop removes the
            // socket on exit).
            app.manage(broker_handle);

            // Remote Control (C1): managed state + event bus. The `drain_sidecar`
            // task publishes run events onto this bus (no-op when no room is
            // ACTIVE). If the feature was left enabled, auto-start the outbound
            // relay agent (FR-001 "app khởi động khi tính năng đang bật").
            let remote_state = RemoteState::default();
            // Register the RemoteBus as its own managed state so the run event
            // tap in `sidecar.rs` (app.try_state::<RemoteBus>()) publishes onto
            // the SAME bus the forwarder subscribes to. Without this the tap
            // finds no bus and never forwards stream/permission events.
            app.manage(remote_state.bus.clone());
            app.manage(remote_state.clone());
            // Shared engine/model/permission-mode selection store: the desktop
            // composer + a remote controller both read/write it so their selectors
            // stay in lock-step (realtime, both ways).
            app.manage(remote::RunMetaStore::default());
            {
                let app_handle = app.handle().clone();
                let db_for_remote = app_handle.state::<db::Db>().inner().clone();
                let registry = app_handle.state::<RunRegistry>().inner().clone();
                let approvals = app_handle
                    .state::<runs::BrokerApprovals>()
                    .inner()
                    .clone();
                tauri::async_runtime::spawn(async move {
                    let enabled = remote::get_setting(&db_for_remote, remote::KEY_ENABLED)
                        .await
                        .map(|v| v == "true")
                        .unwrap_or(false);
                    if enabled {
                        if let Err(e) = remote_state
                            .start(&app_handle, &db_for_remote, &registry, &approvals)
                            .await
                        {
                            tracing::warn!(event = "remote_autostart_failed", error = %e);
                        }
                    }
                });
            }
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
            list_google_accounts,
            add_google_account,
            delete_google_account,
            rename_google_account,
            set_default_google_account,
            google_client_status,
            google_forget_client,
            list_claude_accounts,
            create_claude_account,
            rename_claude_account,
            delete_claude_account,
            set_default_claude_account,
            open_claude_account_login,
            validate_claude_account,
            set_project_claude_account,
            list_google_calendars,
            list_google_calendar_events,
            create_google_calendar_event,
            update_google_calendar_event,
            delete_google_calendar_event,
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
            reorder_projects,
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
            open_in_chrome,
            open_in_folder,
            open_in_terminal,
            fetch_issue,
            fetch_pr,
            refetch_run,
            list_review_requested_prs,
            resolve_project_board,
            list_milestone_board,
            list_milestone_issues,
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
            set_run_claude_account,
            set_run_pinned,
            list_project_files,
            list_dir,
            set_file_tree_watch,
            stop_file_tree_watch,
            read_project_file,
            write_project_file,
            read_file_base64,
            create_dir,
            create_file,
            rename_entry,
            delete_entry,
            copy_entry,
            move_entry,
            create_handoff_run,
            create_session_run,
            get_usage_stats,
            get_work_digest,
            summarize_work_digest,
            cancel_work_summary,
            translate_text,
            cancel_translate,
            prewarm_translate,
            mascot_speak,
            cancel_mascot_speak,
            list_claude_models,
            refresh_claude_models,
            refresh_codex_models,
            get_model_caches,
            backfill_usage,
            reset_usage_stats,
            get_budget_status,
            get_codex_budget_status,
            get_claude_account_budget,
            get_run_budget,
            get_plan_usage,
            get_plan_usage_codex,
            refresh_plan_usage,
            refresh_codex_plan_usage,
            reconcile_claude_sessions,
            reconcile_codex_sessions,
            get_storage_stats,
            clean_storage,
            list_todos,
            add_todo,
            toggle_todo,
            update_todo,
            delete_todo,
            delete_todos,
            clear_completed_todos,
            reorder_todos,
            set_todo_project,
            list_notes,
            add_note,
            update_note,
            set_note_project,
            delete_note,
            delete_notes,
            reorder_notes,
            show_permission_notification,
            show_calendar_reminder,
            remote_set_config,
            remote_enable,
            remote_disable,
            remote_create_session_link,
            remote_end_session,
            remote_reveal_otp,
            remote_set_master_password,
            remote_get_audit,
            remote_clear_audit,
            remote_status,
            remote::meta::set_run_meta,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            use tauri::Manager;

            // Kill every still-running sidecar (and its CLI child) so no orphaned
            // `claude` / `codex` process lingers in the background burning API tokens.
            let kill_all_sidecars = |app_handle: &tauri::AppHandle| {
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
            };

            match event {
                // Closing the MAIN window quits the whole app. On macOS this would
                // otherwise leave the process alive because the frameless,
                // always-on-top mascot pet (and any quick-create / gantt / file
                // pop-outs) keep running — so the hidden sidecars never get killed.
                // Close every auxiliary window, stop all sidecars, then exit.
                tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::CloseRequested { .. },
                    ..
                } if label == "main" => {
                    for (lbl, win) in app_handle.webview_windows() {
                        if lbl != "main" {
                            let _ = win.close();
                        }
                    }
                    kill_all_sidecars(app_handle);
                    app_handle.exit(0);
                }
                // Quit via Cmd+Q / app menu: same sidecar cleanup.
                tauri::RunEvent::ExitRequested { .. } => {
                    kill_all_sidecars(app_handle);
                }
                _ => {}
            }
        });
}
