use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_protocol::config_types::ForcedLoginMethod;

use super::*;

const ACCOUNTS_POPUP_VIEW_ID: &str = "accounts-popup";

impl ChatWidget {
    pub(crate) fn open_accounts_popup(&mut self) {
        let saved_accounts = match codex_login::list_saved_accounts(&self.config.codex_home) {
            Ok(accounts) => accounts,
            Err(error) => {
                self.add_error_message(format!("Failed to load saved accounts: {error}"));
                return;
            }
        };

        let should_load_statuses = !saved_accounts.is_empty();
        self.show_selection_view(self.accounts_popup_params(
            saved_accounts,
            AccountsPopupStatusState::Loading,
            /*initial_selected_idx*/ None,
        ));

        if should_load_statuses {
            self.load_accounts_popup_statuses();
        }
    }

    pub(crate) fn finish_accounts_popup_status_refresh(
        &mut self,
        saved_accounts: Vec<codex_login::SavedAccountStatus>,
    ) {
        let initial_selected_idx = self
            .bottom_pane
            .selected_index_for_active_view(ACCOUNTS_POPUP_VIEW_ID);
        let params = self.accounts_popup_params(
            saved_accounts,
            AccountsPopupStatusState::Loaded,
            initial_selected_idx,
        );
        self.bottom_pane
            .replace_selection_view_if_active(ACCOUNTS_POPUP_VIEW_ID, params);
    }

    pub(crate) fn fail_accounts_popup_status_refresh(&mut self, error: String) {
        let saved_accounts = match codex_login::list_saved_accounts(&self.config.codex_home) {
            Ok(accounts) => accounts,
            Err(error) => {
                self.add_error_message(format!("Failed to load saved accounts: {error}"));
                return;
            }
        };
        let initial_selected_idx = self
            .bottom_pane
            .selected_index_for_active_view(ACCOUNTS_POPUP_VIEW_ID);
        let params = self.accounts_popup_params(
            saved_accounts,
            AccountsPopupStatusState::Failed(error),
            initial_selected_idx,
        );
        self.bottom_pane
            .replace_selection_view_if_active(ACCOUNTS_POPUP_VIEW_ID, params);
    }

    pub(crate) fn open_account_actions_popup(&mut self, account_key: String) {
        let saved_accounts = match codex_login::list_saved_accounts(&self.config.codex_home) {
            Ok(accounts) => accounts,
            Err(error) => {
                self.add_error_message(format!("Failed to load saved accounts: {error}"));
                return;
            }
        };
        let Some(account) = saved_accounts
            .into_iter()
            .find(|account| account.key == account_key)
        else {
            self.add_error_message("Saved account no longer exists.".to_string());
            return;
        };

        let switch_account_key = account.key.clone();
        let switch_account_label = account.label.clone();
        let switch_codex_home = self.config.codex_home.clone();
        let switch_credentials_store_mode = self.config.cli_auth_credentials_store_mode;
        let switch_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            match codex_login::switch_active_account(
                &switch_codex_home,
                switch_credentials_store_mode,
                switch_account_key.as_str(),
            ) {
                Ok(()) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            format!(
                                "Switched active account to {switch_account_label}. Restarting Codex."
                            ),
                            /*hint*/ None,
                        ),
                    )));
                    tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
                }
                Err(error) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to switch account to {switch_account_label}: {error}"
                        )),
                    )));
                }
            }
        })];

        let remove_account_key = account.key.clone();
        let remove_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenRemoveAccountConfirmation {
                account_key: remove_account_key.clone(),
            });
        })];

        let items = vec![
            SelectionItem {
                name: format!("Switch to {}", account.label),
                description: Some("Make this the active account".to_string()),
                selected_description: account
                    .is_active
                    .then(|| "This account is already active".to_string()),
                is_disabled: account.is_active,
                disabled_reason: account.is_active.then(|| "Already active".to_string()),
                actions: switch_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: format!("Remove {}", account.label),
                description: Some("Delete this saved login from /accounts".to_string()),
                actions: remove_actions,
                dismiss_on_select: false,
                ..Default::default()
            },
            SelectionItem {
                name: "Back".to_string(),
                description: Some("Return to the accounts list".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.show_selection_view(SelectionViewParams {
            view_id: Some("account-actions-popup"),
            title: Some("Account".to_string()),
            subtitle: Some(account.label),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(if account.is_active { 1 } else { 0 }),
            ..Default::default()
        });
    }

    pub(crate) fn open_remove_account_confirmation(&mut self, account_key: String) {
        let saved_accounts = match codex_login::list_saved_accounts(&self.config.codex_home) {
            Ok(accounts) => accounts,
            Err(error) => {
                self.add_error_message(format!("Failed to load saved accounts: {error}"));
                return;
            }
        };
        let Some(account) = saved_accounts
            .into_iter()
            .find(|account| account.key == account_key)
        else {
            self.add_error_message("Saved account no longer exists.".to_string());
            return;
        };

        let remove_codex_home = self.config.codex_home.clone();
        let remove_credentials_store_mode = self.config.cli_auth_credentials_store_mode;
        let remove_account_key = account.key.clone();
        let remove_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            match codex_login::remove_saved_account(
                &remove_codex_home,
                remove_credentials_store_mode,
                remove_account_key.as_str(),
            ) {
                Ok(codex_login::RemoveSavedAccountResult::RemovedInactive { removed_label }) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            format!("Removed saved account {removed_label}."),
                            /*hint*/ None,
                        ),
                    )));
                    tx.send(AppEvent::DismissBottomPaneViews);
                }
                Ok(codex_login::RemoveSavedAccountResult::RemovedActiveSwitched {
                    removed_label,
                    replacement_label,
                }) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            format!(
                                "Removed {removed_label}. Switched active account to {replacement_label}. Restarting Codex."
                            ),
                            /*hint*/ None,
                        ),
                    )));
                    tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
                }
                Ok(codex_login::RemoveSavedAccountResult::RemovedLastActive { removed_label }) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            format!(
                                "Removed {removed_label}. No saved accounts remain. Logging out and restarting Codex."
                            ),
                            /*hint*/ None,
                        ),
                    )));
                    tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
                }
                Err(error) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to remove saved account: {error}"
                        )),
                    )));
                }
            }
        })];

        let items = vec![
            SelectionItem {
                name: "Remove account".to_string(),
                description: Some(if account.is_active {
                    "Remove this active account and restart Codex".to_string()
                } else {
                    "Remove this saved account".to_string()
                }),
                actions: remove_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Keep this saved account".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.show_selection_view(SelectionViewParams {
            view_id: Some("remove-account-confirmation-popup"),
            title: Some("Remove account?".to_string()),
            subtitle: Some(account.label),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }
}

trait AccountsPopupAccount {
    fn summary(&self) -> &codex_login::SavedAccountSummary;
    fn status_description(&self, state: &AccountsPopupStatusState) -> String;
    fn display_name(&self) -> String;
}

impl AccountsPopupAccount for codex_login::SavedAccountSummary {
    fn summary(&self) -> &codex_login::SavedAccountSummary {
        self
    }

    fn status_description(&self, state: &AccountsPopupStatusState) -> String {
        match state {
            AccountsPopupStatusState::Loading => "loading limits...".to_string(),
            AccountsPopupStatusState::Loaded => {
                saved_account_mode_label(self.auth_mode).to_string()
            }
            AccountsPopupStatusState::Failed(error) => format!("failed to load limits: {error}"),
        }
    }

    fn display_name(&self) -> String {
        self.label.clone()
    }
}

impl AccountsPopupAccount for codex_login::SavedAccountStatus {
    fn summary(&self) -> &codex_login::SavedAccountSummary {
        &self.summary
    }

    fn status_description(&self, _state: &AccountsPopupStatusState) -> String {
        match &self.rate_limits {
            codex_login::SavedAccountRateLimits::Available(rate_limits) => {
                crate::status::render_saved_account_rate_limit_summary(
                    rate_limits,
                    crate::status::SavedAccountRateLimitSummaryMode::WithResets,
                )
            }
            codex_login::SavedAccountRateLimits::Unsupported { reason } => reason.clone(),
            codex_login::SavedAccountRateLimits::Unavailable { error } => {
                format!("failed to load limits: {error}")
            }
        }
    }

    fn display_name(&self) -> String {
        match (&self.email, self.plan_type) {
            (Some(email), Some(plan_type)) => {
                format!(
                    "{email} ({})",
                    crate::status::plan_type_display_name(plan_type)
                )
            }
            (Some(email), None) => email.clone(),
            (None, Some(plan_type)) => {
                format!(
                    "{} ({})",
                    self.summary.label,
                    crate::status::plan_type_display_name(plan_type)
                )
            }
            (None, None) => self.summary.label.clone(),
        }
    }
}

enum AccountsPopupStatusState {
    Loading,
    Loaded,
    Failed(String),
}

impl ChatWidget {
    fn load_accounts_popup_statuses(&mut self) {
        let tx = self.app_event_tx.clone();
        let codex_home = self.config.codex_home.clone();
        let chatgpt_base_url = Some(self.config.chatgpt_base_url.clone());
        tokio::spawn(async move {
            let result = codex_login::list_saved_account_statuses(&codex_home, chatgpt_base_url)
                .await
                .map_err(|error| error.to_string());
            tx.send(AppEvent::AccountsPopupStatusesLoaded { result });
        });
    }

    fn accounts_popup_params<T: AccountsPopupAccount>(
        &self,
        saved_accounts: Vec<T>,
        state: AccountsPopupStatusState,
        initial_selected_idx: Option<usize>,
    ) -> SelectionViewParams {
        let mut items: Vec<SelectionItem> = saved_accounts
            .into_iter()
            .map(|account| account_item(account, &state))
            .collect();

        items.push(add_account_item(
            self.config.codex_home.to_path_buf(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.forced_login_method,
            self.config.cli_auth_credentials_store_mode,
        ));

        SelectionViewParams {
            view_id: Some(ACCOUNTS_POPUP_VIEW_ID),
            title: Some("Accounts".to_string()),
            subtitle: Some(
                "Switch the active account, remove a saved login, or add another account"
                    .to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        }
    }
}

fn account_item<T: AccountsPopupAccount>(
    account: T,
    state: &AccountsPopupStatusState,
) -> SelectionItem {
    let summary = account.summary();
    let account_key = summary.key.clone();
    let mode_label = saved_account_mode_label(summary.auth_mode);
    let status_description = account.status_description(state);
    let description = if summary.is_active {
        format!("Active · {mode_label} · {status_description}")
    } else {
        format!("{mode_label} · {status_description}")
    };

    SelectionItem {
        name: account.display_name(),
        description: Some(description),
        selected_description: summary
            .is_active
            .then(|| format!("Currently active account · {mode_label} · {status_description}")),
        is_current: summary.is_active,
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenAccountActionsPopup {
                account_key: account_key.clone(),
            });
        })],
        dismiss_on_select: false,
        ..Default::default()
    }
}

fn add_account_item(
    codex_home: PathBuf,
    forced_chatgpt_workspace_id: Option<String>,
    forced_login_method: Option<ForcedLoginMethod>,
    credentials_store_mode: codex_login::AuthCredentialsStoreMode,
) -> SelectionItem {
    let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
        if matches!(forced_login_method, Some(ForcedLoginMethod::Api)) {
            tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_error_event(
                    "This environment requires API key login. Use `printenv OPENAI_API_KEY | codex login --with-api-key` in a terminal."
                        .to_string(),
                ),
            )));
            return;
        }

        tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_info_event(
                "Opening browser login to add another account...".to_string(),
                /*hint*/ None,
            ),
        )));

        let tx = tx.clone();
        let codex_home = codex_home.clone();
        let forced_chatgpt_workspace_id = forced_chatgpt_workspace_id.clone();
        tokio::spawn(async move {
            let opts = codex_login::ServerOptions::new(
                codex_home,
                codex_login::CLIENT_ID.to_string(),
                forced_chatgpt_workspace_id,
                credentials_store_mode,
            );
            let result = match codex_login::run_login_server(opts) {
                Ok(server) => server.block_until_done().await,
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(history_cell::new_info_event(
                        "Account login complete. The new account is now active. Restarting Codex."
                            .to_string(),
                        /*hint*/ None,
                    ))));
                    tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
                }
                Err(error) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!("Account login failed: {error}")),
                    )));
                }
            }
        });
    })];

    SelectionItem {
        name: "Add account".to_string(),
        description: Some("Sign in with another account in your browser".to_string()),
        actions,
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn saved_account_mode_label(auth_mode: ApiAuthMode) -> &'static str {
    match auth_mode {
        ApiAuthMode::ApiKey => "API key",
        ApiAuthMode::Chatgpt => "ChatGPT",
        ApiAuthMode::ChatgptAuthTokens => "ChatGPT (external)",
        ApiAuthMode::AgentIdentity => "Agent identity",
    }
}
