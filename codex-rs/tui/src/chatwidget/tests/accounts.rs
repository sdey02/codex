use super::*;
use codex_app_server_protocol::AuthMode;
use codex_login::AuthDotJson;
use codex_login::SavedAccountRateLimits;
use codex_login::SavedAccountStatus;
use codex_login::SavedAccountSummary;
use pretty_assertions::assert_eq;

fn api_key_auth(api_key: &str) -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
    }
}

fn save_api_key_account(chat: &ChatWidget, api_key: &str) {
    codex_login::save_auth(
        &chat.config.codex_home,
        &api_key_auth(api_key),
        chat.config.cli_auth_credentials_store_mode,
    )
    .expect("save auth");
}

fn saved_account_key(chat: &ChatWidget, suffix: &str) -> String {
    let needle = format!("...{suffix})");
    codex_login::list_saved_accounts(&chat.config.codex_home)
        .expect("list saved accounts")
        .into_iter()
        .find(|account| account.label.ends_with(&needle))
        .unwrap_or_else(|| panic!("missing saved account for suffix {suffix}"))
        .key
}

fn saved_account_labels(chat: &ChatWidget) -> Vec<(String, bool)> {
    codex_login::list_saved_accounts(&chat.config.codex_home)
        .expect("list saved accounts")
        .into_iter()
        .map(|account| (account.label, account.is_active))
        .collect()
}

fn saved_status(
    key: &str,
    label: &str,
    auth_mode: AuthMode,
    is_active: bool,
    email: Option<&str>,
    plan_type: Option<codex_protocol::account::PlanType>,
    rate_limits: SavedAccountRateLimits,
) -> SavedAccountStatus {
    SavedAccountStatus {
        summary: SavedAccountSummary {
            key: key.to_string(),
            label: label.to_string(),
            auth_mode,
            is_active,
        },
        email: email.map(str::to_string),
        plan_type,
        rate_limits,
    }
}

fn rate_limit_snapshot(primary_percent: f64, secondary_percent: f64) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: primary_percent,
            window_minutes: Some(300),
            resets_at: None,
        }),
        secondary: Some(RateLimitWindow {
            used_percent: secondary_percent,
            window_minutes: Some(10_080),
            resets_at: None,
        }),
        credits: None,
        plan_type: Some(codex_protocol::account::PlanType::Pro),
        rate_limit_reached_type: None,
    }
}

fn open_account_actions_from_popup(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let account_key = match rx.try_recv() {
        Ok(AppEvent::OpenAccountActionsPopup { account_key }) => account_key,
        other => panic!("expected account actions popup event, got {other:?}"),
    };
    chat.open_account_actions_popup(account_key);
}

fn open_remove_confirmation_from_actions_popup(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let account_key = match rx.try_recv() {
        Ok(AppEvent::OpenRemoveAccountConfirmation { account_key }) => account_key,
        other => panic!("expected remove confirmation event, got {other:?}"),
    };
    chat.open_remove_account_confirmation(account_key);
}

#[tokio::test]
async fn accounts_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");
    save_api_key_account(&chat, "sk-test-key-2222");

    chat.dispatch_command(SlashCommand::Accounts);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("accounts_popup", popup);
}

#[tokio::test]
async fn accounts_popup_loaded_status_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.dispatch_command(SlashCommand::Accounts);
    chat.finish_accounts_popup_status_refresh(vec![
        saved_status(
            "chatgpt:active",
            "active@example.com",
            AuthMode::Chatgpt,
            /*is_active*/ true,
            Some("active@example.com"),
            Some(codex_protocol::account::PlanType::Pro),
            SavedAccountRateLimits::Available(vec![rate_limit_snapshot(
                /*primary_percent*/ 25.0, /*secondary_percent*/ 80.0,
            )]),
        ),
        saved_status(
            "chatgpt:other",
            "other@example.com",
            AuthMode::Chatgpt,
            /*is_active*/ false,
            Some("other@example.com"),
            Some(codex_protocol::account::PlanType::Plus),
            SavedAccountRateLimits::Available(vec![rate_limit_snapshot(
                /*primary_percent*/ 60.0, /*secondary_percent*/ 10.0,
            )]),
        ),
        saved_status(
            "api:key",
            "API key (...1111)",
            AuthMode::ApiKey,
            /*is_active*/ false,
            None,
            None,
            SavedAccountRateLimits::Unsupported {
                reason: "limits unavailable for API key auth".to_string(),
            },
        ),
        saved_status(
            "chatgpt:expired",
            "expired@example.com",
            AuthMode::Chatgpt,
            /*is_active*/ false,
            Some("expired@example.com"),
            Some(codex_protocol::account::PlanType::Pro),
            SavedAccountRateLimits::Unavailable {
                error: "credentials expired".to_string(),
            },
        ),
    ]);

    let popup = render_bottom_popup(&chat, /*width*/ 110);
    assert_chatwidget_snapshot!("accounts_popup_loaded_status", popup);
}

#[tokio::test]
async fn accounts_popup_failed_status_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");

    chat.dispatch_command(SlashCommand::Accounts);
    chat.fail_accounts_popup_status_refresh("network unavailable".to_string());

    let popup = render_bottom_popup(&chat, /*width*/ 90);
    assert_chatwidget_snapshot!("accounts_popup_failed_status", popup);
}

#[tokio::test]
async fn account_actions_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");
    save_api_key_account(&chat, "sk-test-key-2222");

    chat.open_account_actions_popup(saved_account_key(&chat, "2222"));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("account_actions_popup", popup);
}

#[tokio::test]
async fn remove_inactive_account_emits_dismiss_without_restart() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");
    save_api_key_account(&chat, "sk-test-key-2222");

    chat.dispatch_command(SlashCommand::Accounts);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    open_account_actions_from_popup(&mut chat, &mut rx);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    open_remove_confirmation_from_actions_popup(&mut chat, &mut rx);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => lines_to_single_string(&cell.display_lines(80)),
        other => panic!("expected history event after inactive removal, got {other:?}"),
    };
    assert!(
        rendered.contains("Removed saved account API key (...1111)."),
        "expected inactive removal message, got: {rendered}"
    );
    assert_matches!(rx.try_recv(), Ok(AppEvent::DismissBottomPaneViews));
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(event, AppEvent::Exit(_))),
        "inactive removal should not request restart"
    );
    assert_eq!(
        saved_account_labels(&chat),
        vec![("API key (...2222)".to_string(), true)]
    );
}

#[tokio::test]
async fn remove_active_account_restarts_with_replacement() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");
    save_api_key_account(&chat, "sk-test-key-2222");

    chat.dispatch_command(SlashCommand::Accounts);
    open_account_actions_from_popup(&mut chat, &mut rx);
    open_remove_confirmation_from_actions_popup(&mut chat, &mut rx);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => lines_to_single_string(&cell.display_lines(80)),
        other => panic!("expected history event after active removal, got {other:?}"),
    };
    assert!(
        rendered.contains(
            "Removed API key (...2222). Switched active account to API key (...1111). Restarting Codex."
        ),
        "expected active removal message, got: {rendered}"
    );
    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::ShutdownFirst)));
    assert_eq!(
        saved_account_labels(&chat),
        vec![("API key (...1111)".to_string(), true)]
    );
    let active_auth = codex_login::load_auth_dot_json(
        &chat.config.codex_home,
        chat.config.cli_auth_credentials_store_mode,
    )
    .expect("load auth")
    .expect("active auth");
    assert_eq!(active_auth, api_key_auth("sk-test-key-1111"));
}

#[tokio::test]
async fn remove_last_active_account_restarts_logged_out() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    save_api_key_account(&chat, "sk-test-key-1111");

    chat.dispatch_command(SlashCommand::Accounts);
    open_account_actions_from_popup(&mut chat, &mut rx);
    open_remove_confirmation_from_actions_popup(&mut chat, &mut rx);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => lines_to_single_string(&cell.display_lines(80)),
        other => panic!("expected history event after removing last account, got {other:?}"),
    };
    assert!(
        rendered.contains(
            "Removed API key (...1111). No saved accounts remain. Logging out and restarting Codex."
        ),
        "expected logout message, got: {rendered}"
    );
    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::ShutdownFirst)));
    assert!(saved_account_labels(&chat).is_empty());
    assert_eq!(
        codex_login::load_auth_dot_json(
            &chat.config.codex_home,
            chat.config.cli_auth_credentials_store_mode,
        )
        .expect("load auth"),
        None
    );
}
