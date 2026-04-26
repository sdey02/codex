use std::collections::BTreeSet;

use chrono::Local;
use codex_login::SavedAccountRateLimits;
use codex_login::SavedAccountStatus;
use ratatui::prelude::*;
use textwrap::Options;

use super::format::FieldFormatter;
use super::format::push_label;
use super::helpers::plan_type_display_name;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::StatusRateLimitRow;
use super::rate_limits::StatusRateLimitValue;
use super::rate_limits::compose_rate_limit_data_many;
use super::rate_limits::format_status_limit_summary;
use super::rate_limits::rate_limit_snapshot_display_for_limit;

#[derive(Debug, Clone)]
pub(crate) enum SavedAccountsState {
    Hidden,
    Loading,
    Failed(String),
    Loaded(Vec<SavedAccountStatus>),
}

pub(crate) fn collect_saved_account_labels(
    state: &SavedAccountsState,
    seen: &mut BTreeSet<String>,
    labels: &mut Vec<String>,
) {
    if renderable_accounts(state).is_some() {
        push_label(labels, seen, "Other accounts");
    }
}

pub(crate) fn render_saved_account_lines(
    state: &SavedAccountsState,
    available_inner_width: usize,
    formatter: &FieldFormatter,
) -> Vec<Line<'static>> {
    let Some(accounts) = renderable_accounts(state) else {
        return Vec::new();
    };
    let value_width = formatter.value_width(available_inner_width).max(1);
    let wrap_options = Options::new(value_width).break_words(false);
    let mut lines = Vec::new();

    for account in accounts {
        let rendered = render_saved_account(account);
        for wrapped in textwrap::wrap(rendered.as_str(), wrap_options.clone()) {
            let span = Span::from(wrapped.into_owned());
            if lines.is_empty() {
                lines.push(formatter.line("Other accounts", vec![span]));
            } else {
                lines.push(formatter.continuation(vec![span]));
            }
        }
    }

    lines
}

fn renderable_accounts(state: &SavedAccountsState) -> Option<Vec<RenderedSavedAccount<'_>>> {
    match state {
        SavedAccountsState::Hidden => None,
        SavedAccountsState::Loading => Some(vec![RenderedSavedAccount::Message(
            "loading...".to_string(),
        )]),
        SavedAccountsState::Failed(error) => Some(vec![RenderedSavedAccount::Message(format!(
            "failed to load saved accounts: {error}"
        ))]),
        SavedAccountsState::Loaded(accounts) => {
            let accounts: Vec<RenderedSavedAccount<'_>> = accounts
                .iter()
                .filter(|account| !account.summary.is_active)
                .map(RenderedSavedAccount::Account)
                .collect();
            (!accounts.is_empty()).then_some(accounts)
        }
    }
}

fn render_saved_account(account: RenderedSavedAccount<'_>) -> String {
    match account {
        RenderedSavedAccount::Message(message) => message,
        RenderedSavedAccount::Account(account) => {
            let name = account_display_name(account);
            let detail = match &account.rate_limits {
                SavedAccountRateLimits::Available(rate_limits) => render_rate_limit_summary(
                    rate_limits,
                    SavedAccountRateLimitSummaryMode::Compact,
                ),
                SavedAccountRateLimits::Unsupported { reason } => reason.clone(),
                SavedAccountRateLimits::Unavailable { error } => {
                    format!("failed to load limits: {error}")
                }
            };
            format!("{name} · {detail}")
        }
    }
}

fn account_display_name(account: &SavedAccountStatus) -> String {
    match (&account.email, account.plan_type) {
        (Some(email), Some(plan_type)) => {
            format!("{email} ({})", plan_type_display_name(plan_type))
        }
        (Some(email), None) => email.clone(),
        (None, Some(plan_type)) => {
            format!(
                "{} ({})",
                account.summary.label,
                plan_type_display_name(plan_type)
            )
        }
        (None, None) => account.summary.label.clone(),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SavedAccountRateLimitSummaryMode {
    Compact,
    WithResets,
}

pub(crate) fn render_rate_limit_summary(
    rate_limits: &[codex_protocol::protocol::RateLimitSnapshot],
    mode: SavedAccountRateLimitSummaryMode,
) -> String {
    let now = Local::now();
    let displays: Vec<_> = rate_limits
        .iter()
        .map(|snapshot| {
            rate_limit_snapshot_display_for_limit(
                snapshot,
                snapshot
                    .limit_name
                    .clone()
                    .or(snapshot.limit_id.clone())
                    .unwrap_or_else(|| "codex".to_string()),
                now,
            )
        })
        .collect();

    match compose_rate_limit_data_many(displays.as_slice(), now) {
        StatusRateLimitData::Available(rows) => {
            render_rate_limit_rows(rows.as_slice(), /*stale*/ false, mode)
        }
        StatusRateLimitData::Stale(rows) => {
            render_rate_limit_rows(rows.as_slice(), /*stale*/ true, mode)
        }
        StatusRateLimitData::Unavailable | StatusRateLimitData::Missing => {
            "limits unavailable".to_string()
        }
    }
}

fn render_rate_limit_rows(
    rows: &[StatusRateLimitRow],
    stale: bool,
    mode: SavedAccountRateLimitSummaryMode,
) -> String {
    let mut parts: Vec<String> = rows
        .iter()
        .filter_map(|row| match &row.value {
            StatusRateLimitValue::Window {
                percent_used,
                resets_at,
            } => {
                let mut text = format!(
                    "{} {}",
                    row.label.to_ascii_lowercase(),
                    format_status_limit_summary((100.0 - percent_used).clamp(0.0, 100.0))
                );
                if matches!(mode, SavedAccountRateLimitSummaryMode::WithResets)
                    && let Some(resets_at) = resets_at
                {
                    text.push_str(format!(", resets {resets_at}").as_str());
                }
                Some(text)
            }
            StatusRateLimitValue::Text(text) if !text.is_empty() => {
                Some(format!("{} {}", row.label.to_ascii_lowercase(), text))
            }
            StatusRateLimitValue::Text(_) => None,
        })
        .collect();
    if stale {
        parts.push("may be stale".to_string());
    }
    if parts.is_empty() {
        "limits unavailable".to_string()
    } else {
        parts.join(" · ")
    }
}

#[derive(Debug, Clone)]
enum RenderedSavedAccount<'a> {
    Message(String),
    Account(&'a SavedAccountStatus),
}
