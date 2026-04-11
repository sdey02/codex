use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_protocol::config_types::ForcedLoginMethod;

use super::*;

impl ChatWidget {
    pub(crate) fn open_accounts_popup(&mut self) {
        let saved_accounts = match codex_login::list_saved_accounts(&self.config.codex_home) {
            Ok(accounts) => accounts,
            Err(error) => {
                self.add_error_message(format!("Failed to load saved accounts: {error}"));
                return;
            }
        };

        let mut items: Vec<SelectionItem> = Vec::new();

        for account in saved_accounts {
            let codex_home = self.config.codex_home.clone();
            let account_key = account.key.clone();
            let account_label = account.label.clone();
            let auth_mode = account.auth_mode;
            let credentials_store_mode = self.config.cli_auth_credentials_store_mode;
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                match codex_login::switch_active_account(
                    &codex_home,
                    credentials_store_mode,
                    account_key.as_str(),
                ) {
                    Ok(()) => {
                        tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_info_event(
                                format!(
                                    "Switched active account to {account_label}. Restarting Codex."
                                ),
                                /*hint*/ None,
                            ),
                        )));
                        tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
                    }
                    Err(error) => {
                        tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_error_event(format!(
                                "Failed to switch account to {account_label}: {error}"
                            )),
                        )));
                    }
                }
            })];

            let mode_label = match auth_mode {
                ApiAuthMode::ApiKey => "API key",
                ApiAuthMode::Chatgpt => "ChatGPT",
                ApiAuthMode::ChatgptAuthTokens => "ChatGPT (external)",
            };
            let selected_description = account
                .is_active
                .then(|| "Currently active account".to_string());

            items.push(SelectionItem {
                name: account.label,
                description: Some(mode_label.to_string()),
                selected_description,
                is_current: account.is_active,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let codex_home = self.config.codex_home.clone();
        let forced_chatgpt_workspace_id = self.config.forced_chatgpt_workspace_id.clone();
        let forced_login_method = self.config.forced_login_method;
        let credentials_store_mode = self.config.cli_auth_credentials_store_mode;
        let add_account_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            if matches!(forced_login_method, Some(ForcedLoginMethod::Api)) {
                tx.send(AppEvent::InsertHistoryCell(Box::new(history_cell::new_error_event(
                    "This environment requires API key login. Use `printenv OPENAI_API_KEY | codex login --with-api-key` in a terminal."
                        .to_string(),
                ))));
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

        items.push(SelectionItem {
            name: "Add account".to_string(),
            description: Some("Sign in with another account in your browser".to_string()),
            selected_description: None,
            actions: add_account_actions,
            dismiss_on_select: true,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Accounts".to_string()),
            subtitle: Some("Switch active account or add another login".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }
}
