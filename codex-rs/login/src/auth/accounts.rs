use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_backend_openapi_models::models::CreditStatusDetails;
use codex_backend_openapi_models::models::PlanType as BackendPlanType;
use codex_backend_openapi_models::models::RateLimitReachedKind as BackendRateLimitReachedKind;
use codex_backend_openapi_models::models::RateLimitStatusDetails;
use codex_backend_openapi_models::models::RateLimitStatusPayload;
use codex_backend_openapi_models::models::RateLimitWindowSnapshot;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::RateLimitReachedType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tokio::task::JoinSet;
use tokio::time::timeout;

use super::manager::CLIENT_ID;
use super::manager::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use super::storage::AuthDotJson;
use crate::auth::default_client::build_reqwest_client;
use crate::auth::default_client::get_codex_user_agent;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_jwt_expiration;

const ACCOUNTS_FILE_NAME: &str = "accounts.json";
const ACCOUNTS_FILE_VERSION: u8 = 1;
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const TOKEN_REFRESH_INTERVAL_DAYS: i64 = 8;
const SAVED_ACCOUNT_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAccountSummary {
    pub key: String,
    pub label: String,
    pub auth_mode: ApiAuthMode,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedAccountStatus {
    pub summary: SavedAccountSummary,
    pub email: Option<String>,
    pub plan_type: Option<AccountPlanType>,
    pub rate_limits: SavedAccountRateLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SavedAccountRateLimits {
    Available(Vec<RateLimitSnapshot>),
    Unsupported { reason: String },
    Unavailable { error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum RemoveSavedAccountResultInternal {
    Inactive {
        removed: SavedAccountSummary,
    },
    ActiveSwitched {
        removed: SavedAccountSummary,
        replacement: SavedAccountSummary,
        replacement_auth: Box<AuthDotJson>,
    },
    LastActive {
        removed: SavedAccountSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedAccountEntry {
    key: String,
    label: String,
    auth_mode: ApiAuthMode,
    auth: AuthDotJson,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedAccountsFile {
    #[serde(default = "saved_accounts_file_version")]
    version: u8,
    #[serde(default)]
    active_account_key: Option<String>,
    #[serde(default)]
    accounts: Vec<SavedAccountEntry>,
}

impl Default for SavedAccountsFile {
    fn default() -> Self {
        Self {
            version: ACCOUNTS_FILE_VERSION,
            active_account_key: None,
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct FetchedSavedAccountStatus {
    status: SavedAccountStatus,
    updated_auth: Option<AuthDotJson>,
}

#[derive(Debug, Serialize)]
struct RefreshRequest {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Debug)]
enum UsageFetchError {
    Unauthorized,
    Other(String),
}

const fn saved_accounts_file_version() -> u8 {
    ACCOUNTS_FILE_VERSION
}

pub(super) fn upsert_saved_account(codex_home: &Path, auth: &AuthDotJson) -> std::io::Result<()> {
    let auth_mode = resolved_auth_mode(auth);
    if auth_mode == ApiAuthMode::ChatgptAuthTokens {
        return Ok(());
    }

    let now = Utc::now();
    let (key, label) = account_identity(auth, auth_mode)?;
    let mut saved = read_saved_accounts(codex_home)?;

    saved.accounts.retain(|existing| existing.key != key);
    saved.accounts.push(SavedAccountEntry {
        key: key.clone(),
        label,
        auth_mode,
        auth: auth.clone(),
        updated_at: now,
    });
    saved.active_account_key = Some(key);
    saved.accounts = sort_entries(&saved);

    write_saved_accounts(codex_home, &saved)
}

pub fn list_saved_accounts(codex_home: &Path) -> std::io::Result<Vec<SavedAccountSummary>> {
    let saved = read_saved_accounts(codex_home)?;
    Ok(sort_entries(&saved)
        .into_iter()
        .map(|entry| summary_from_entry(&entry, saved.active_account_key.as_deref()))
        .collect())
}

pub async fn list_saved_account_statuses(
    codex_home: &Path,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<Vec<SavedAccountStatus>> {
    let mut saved = read_saved_accounts(codex_home)?;
    let ordered = sort_entries(&saved);
    if ordered.is_empty() {
        return Ok(Vec::new());
    }

    let mut statuses: Vec<Option<SavedAccountStatus>> = vec![None; ordered.len()];
    let mut join_set: JoinSet<(usize, FetchedSavedAccountStatus)> = JoinSet::new();
    let http = build_reqwest_client();

    for (index, entry) in ordered.iter().enumerate() {
        let summary = summary_from_entry(entry, saved.active_account_key.as_deref());
        let email = account_email(&entry.auth);
        let plan_type = account_plan_type(&entry.auth);

        match entry.auth_mode {
            ApiAuthMode::ApiKey => {
                statuses[index] = Some(SavedAccountStatus {
                    summary,
                    email,
                    plan_type,
                    rate_limits: SavedAccountRateLimits::Unsupported {
                        reason: "limits unavailable for API key auth".to_string(),
                    },
                });
            }
            ApiAuthMode::AgentIdentity => {
                statuses[index] = Some(SavedAccountStatus {
                    summary,
                    email,
                    plan_type,
                    rate_limits: SavedAccountRateLimits::Unsupported {
                        reason: "limits unavailable for agent identity auth".to_string(),
                    },
                });
            }
            ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => {
                let entry = entry.clone();
                let base_url = chatgpt_base_url.clone();
                let http = http.clone();
                join_set.spawn(async move {
                    let fetched = timeout(
                        SAVED_ACCOUNT_STATUS_TIMEOUT,
                        fetch_saved_chatgpt_status(http, entry, base_url, summary.clone()),
                    )
                    .await
                    .unwrap_or_else(|_| FetchedSavedAccountStatus {
                        status: SavedAccountStatus {
                            summary,
                            email,
                            plan_type,
                            rate_limits: SavedAccountRateLimits::Unavailable {
                                error: "request timed out".to_string(),
                            },
                        },
                        updated_auth: None,
                    });
                    (index, fetched)
                });
            }
        }
    }

    while let Some(result) = join_set.join_next().await {
        let (index, fetched) = result
            .map_err(|error| std::io::Error::other(format!("status task failed: {error}")))?;
        if let Some(updated_auth) = fetched.updated_auth
            && let Some(saved_entry) = saved
                .accounts
                .iter_mut()
                .find(|saved_entry| saved_entry.key == fetched.status.summary.key)
        {
            saved_entry.auth = updated_auth;
            saved_entry.updated_at = Utc::now();
        }
        statuses[index] = Some(fetched.status);
    }

    if saved.accounts != ordered {
        saved.accounts = sort_entries(&saved);
    }
    write_saved_accounts(codex_home, &saved)?;

    statuses
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| std::io::Error::other("missing saved account status"))
}

pub(super) fn switch_saved_account(
    codex_home: &Path,
    account_key: &str,
) -> std::io::Result<AuthDotJson> {
    let mut saved = read_saved_accounts(codex_home)?;
    let Some(index) = saved
        .accounts
        .iter()
        .position(|entry| entry.key == account_key)
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Saved account not found: {account_key}"),
        ));
    };

    let mut entry = saved.accounts.remove(index);
    entry.updated_at = Utc::now();
    let auth = entry.auth.clone();
    saved.active_account_key = Some(entry.key.clone());
    saved.accounts.push(entry);
    saved.accounts = sort_entries(&saved);
    write_saved_accounts(codex_home, &saved)?;
    Ok(auth)
}

pub(super) fn remove_saved_account(
    codex_home: &Path,
    account_key: &str,
) -> std::io::Result<RemoveSavedAccountResultInternal> {
    let mut saved = read_saved_accounts(codex_home)?;
    let Some(index) = saved
        .accounts
        .iter()
        .position(|entry| entry.key == account_key)
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Saved account not found: {account_key}"),
        ));
    };

    let removed = saved.accounts.remove(index);
    let removed_summary = summary_from_entry(&removed, saved.active_account_key.as_deref());

    if saved.active_account_key.as_deref() != Some(account_key) {
        saved.accounts = sort_entries(&saved);
        write_saved_accounts(codex_home, &saved)?;
        return Ok(RemoveSavedAccountResultInternal::Inactive {
            removed: removed_summary,
        });
    }

    let ordered = sort_entries(&saved);
    let Some(mut replacement) = ordered.first().cloned() else {
        saved.active_account_key = None;
        write_saved_accounts(codex_home, &saved)?;
        return Ok(RemoveSavedAccountResultInternal::LastActive {
            removed: removed_summary,
        });
    };

    replacement.updated_at = Utc::now();
    saved.active_account_key = Some(replacement.key.clone());
    saved.accounts = saved
        .accounts
        .into_iter()
        .map(|entry| {
            if entry.key == replacement.key {
                replacement.clone()
            } else {
                entry
            }
        })
        .collect();
    saved.accounts = sort_entries(&saved);
    write_saved_accounts(codex_home, &saved)?;

    Ok(RemoveSavedAccountResultInternal::ActiveSwitched {
        removed: removed_summary,
        replacement: summary_from_entry(&replacement, saved.active_account_key.as_deref()),
        replacement_auth: Box::new(replacement.auth),
    })
}

fn resolved_auth_mode(auth: &AuthDotJson) -> ApiAuthMode {
    if let Some(mode) = auth.auth_mode {
        return mode;
    }
    if auth.openai_api_key.is_some() {
        return ApiAuthMode::ApiKey;
    }
    if auth.agent_identity.is_some() {
        return ApiAuthMode::AgentIdentity;
    }
    ApiAuthMode::Chatgpt
}

fn account_identity(
    auth: &AuthDotJson,
    auth_mode: ApiAuthMode,
) -> std::io::Result<(String, String)> {
    match auth_mode {
        ApiAuthMode::ApiKey => {
            let Some(api_key) = auth.openai_api_key.as_ref() else {
                return Err(std::io::Error::other(
                    "API key auth is missing key material",
                ));
            };
            let suffix = api_key
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            Ok((
                format!("api:{}", short_hash(api_key.as_bytes())),
                format!("API key (...{suffix})"),
            ))
        }
        ApiAuthMode::AgentIdentity => {
            let Some(record) = auth.agent_identity.as_ref() else {
                return Err(std::io::Error::other(
                    "agent identity auth is missing account data",
                ));
            };
            Ok((format!("agent:{}", record.account_id), record.email.clone()))
        }
        ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => {
            let account_id = auth
                .tokens
                .as_ref()
                .and_then(|token_data| token_data.account_id.clone())
                .or_else(|| {
                    auth.tokens
                        .as_ref()
                        .and_then(|token_data| token_data.id_token.chatgpt_account_id.clone())
                });
            let email = auth
                .tokens
                .as_ref()
                .and_then(|token_data| token_data.id_token.email.clone());
            let key = match account_id.as_ref() {
                Some(value) => format!("chatgpt:{value}"),
                None => format!(
                    "chatgpt:{}",
                    short_hash(
                        serde_json::to_vec(auth)
                            .map_err(std::io::Error::other)?
                            .as_slice()
                    )
                ),
            };
            let label = email
                .or(account_id)
                .unwrap_or_else(|| "ChatGPT account".to_string());
            Ok((key, label))
        }
    }
}

fn account_email(auth: &AuthDotJson) -> Option<String> {
    auth.agent_identity
        .as_ref()
        .map(|record| record.email.clone())
        .or_else(|| {
            auth.tokens
                .as_ref()
                .and_then(|token_data| token_data.id_token.email.clone())
        })
}

fn account_plan_type(auth: &AuthDotJson) -> Option<AccountPlanType> {
    if let Some(record) = auth.agent_identity.as_ref() {
        return Some(record.plan_type);
    }

    auth.tokens
        .as_ref()
        .and_then(|token_data| token_data.id_token.chatgpt_plan_type.as_ref())
        .map(map_internal_plan_type)
}

fn map_internal_plan_type(plan_type: &InternalPlanType) -> AccountPlanType {
    match plan_type {
        InternalPlanType::Known(known_plan) => match known_plan {
            InternalKnownPlan::Free => AccountPlanType::Free,
            InternalKnownPlan::Go => AccountPlanType::Go,
            InternalKnownPlan::Plus => AccountPlanType::Plus,
            InternalKnownPlan::Pro => AccountPlanType::Pro,
            InternalKnownPlan::ProLite => AccountPlanType::ProLite,
            InternalKnownPlan::Team => AccountPlanType::Team,
            InternalKnownPlan::SelfServeBusinessUsageBased => {
                AccountPlanType::SelfServeBusinessUsageBased
            }
            InternalKnownPlan::Business => AccountPlanType::Business,
            InternalKnownPlan::EnterpriseCbpUsageBased => AccountPlanType::EnterpriseCbpUsageBased,
            InternalKnownPlan::Enterprise => AccountPlanType::Enterprise,
            InternalKnownPlan::Edu => AccountPlanType::Edu,
        },
        InternalPlanType::Unknown(_) => AccountPlanType::Unknown,
    }
}

fn map_backend_plan_type(plan_type: BackendPlanType) -> AccountPlanType {
    match plan_type {
        BackendPlanType::Guest | BackendPlanType::Free | BackendPlanType::FreeWorkspace => {
            AccountPlanType::Free
        }
        BackendPlanType::Go => AccountPlanType::Go,
        BackendPlanType::Plus => AccountPlanType::Plus,
        BackendPlanType::Pro => AccountPlanType::Pro,
        BackendPlanType::ProLite => AccountPlanType::ProLite,
        BackendPlanType::Team => AccountPlanType::Team,
        BackendPlanType::SelfServeBusinessUsageBased => {
            AccountPlanType::SelfServeBusinessUsageBased
        }
        BackendPlanType::Business => AccountPlanType::Business,
        BackendPlanType::EnterpriseCbpUsageBased => AccountPlanType::EnterpriseCbpUsageBased,
        BackendPlanType::Enterprise => AccountPlanType::Enterprise,
        BackendPlanType::Education | BackendPlanType::Edu => AccountPlanType::Edu,
        BackendPlanType::Quorum | BackendPlanType::K12 | BackendPlanType::Unknown => {
            AccountPlanType::Unknown
        }
    }
}

fn summary_from_entry(
    entry: &SavedAccountEntry,
    active_account_key: Option<&str>,
) -> SavedAccountSummary {
    SavedAccountSummary {
        key: entry.key.clone(),
        label: entry.label.clone(),
        auth_mode: entry.auth_mode,
        is_active: active_account_key == Some(entry.key.as_str()),
    }
}

fn sort_entries(saved: &SavedAccountsFile) -> Vec<SavedAccountEntry> {
    let active_key = saved.active_account_key.as_deref();
    let mut accounts = saved.accounts.clone();
    accounts.sort_by(|left, right| {
        let left_is_active = active_key == Some(left.key.as_str());
        let right_is_active = active_key == Some(right.key.as_str());
        right_is_active
            .cmp(&left_is_active)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.label.cmp(&right.label))
    });
    accounts
}

fn short_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    hex[..16].to_string()
}

fn read_saved_accounts(codex_home: &Path) -> std::io::Result<SavedAccountsFile> {
    let file_path = accounts_file_path(codex_home);
    let mut file = match std::fs::File::open(file_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SavedAccountsFile::default());
        }
        Err(error) => return Err(error),
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let mut parsed: SavedAccountsFile = serde_json::from_str(&contents)?;
    if parsed.version != ACCOUNTS_FILE_VERSION {
        parsed.version = ACCOUNTS_FILE_VERSION;
    }
    if parsed.active_account_key.as_ref().is_some_and(|active| {
        !parsed
            .accounts
            .iter()
            .any(|entry| entry.key.as_str() == active.as_str())
    }) {
        parsed.active_account_key = None;
    }
    Ok(parsed)
}

fn write_saved_accounts(codex_home: &Path, saved: &SavedAccountsFile) -> std::io::Result<()> {
    let file_path = accounts_file_path(codex_home);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let serialized = serde_json::to_string_pretty(saved)?;
    let mut options = OpenOptions::new();
    options.truncate(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(file_path)?;
    file.write_all(serialized.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn accounts_file_path(codex_home: &Path) -> PathBuf {
    codex_home.join(ACCOUNTS_FILE_NAME)
}

async fn fetch_saved_chatgpt_status(
    http: reqwest::Client,
    entry: SavedAccountEntry,
    chatgpt_base_url: Option<String>,
    summary: SavedAccountSummary,
) -> FetchedSavedAccountStatus {
    let mut auth = entry.auth.clone();
    let email = account_email(&auth);
    let fallback_plan_type = account_plan_type(&auth);
    let updated_auth =
        match fetch_saved_chatgpt_rate_limits(&http, &mut auth, chatgpt_base_url).await {
            Ok(rate_limits) => {
                let plan_type = rate_limits
                    .iter()
                    .find_map(|snapshot| snapshot.plan_type)
                    .or(fallback_plan_type);
                let updated_auth = (auth != entry.auth).then_some(auth);
                return FetchedSavedAccountStatus {
                    status: SavedAccountStatus {
                        summary,
                        email,
                        plan_type,
                        rate_limits: SavedAccountRateLimits::Available(rate_limits),
                    },
                    updated_auth,
                };
            }
            Err(error) => error,
        };

    FetchedSavedAccountStatus {
        status: SavedAccountStatus {
            summary,
            email,
            plan_type: fallback_plan_type,
            rate_limits: SavedAccountRateLimits::Unavailable {
                error: updated_auth,
            },
        },
        updated_auth: (auth != entry.auth).then_some(auth),
    }
}

async fn fetch_saved_chatgpt_rate_limits(
    http: &reqwest::Client,
    auth: &mut AuthDotJson,
    chatgpt_base_url: Option<String>,
) -> Result<Vec<RateLimitSnapshot>, String> {
    if should_refresh_proactively(auth) {
        refresh_saved_chatgpt_auth(http, auth).await?;
    }

    match request_usage_payload(http, auth, chatgpt_base_url.as_deref()).await {
        Ok(payload) => Ok(rate_limit_snapshots_from_payload(payload)),
        Err(UsageFetchError::Unauthorized) => {
            refresh_saved_chatgpt_auth(http, auth).await?;
            request_usage_payload(http, auth, chatgpt_base_url.as_deref())
                .await
                .map(rate_limit_snapshots_from_payload)
                .map_err(|error| match error {
                    UsageFetchError::Unauthorized => "credentials expired".to_string(),
                    UsageFetchError::Other(error) => error,
                })
        }
        Err(UsageFetchError::Other(error)) => Err(error),
    }
}

fn should_refresh_proactively(auth: &AuthDotJson) -> bool {
    if let Some(tokens) = auth.tokens.as_ref()
        && let Ok(Some(expires_at)) = parse_jwt_expiration(&tokens.access_token)
    {
        return expires_at <= Utc::now();
    }

    let Some(last_refresh) = auth.last_refresh else {
        return false;
    };
    last_refresh < Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL_DAYS)
}

async fn refresh_saved_chatgpt_auth(
    http: &reqwest::Client,
    auth: &mut AuthDotJson,
) -> Result<(), String> {
    let Some(tokens) = auth.tokens.as_mut() else {
        return Err("token data is not available".to_string());
    };
    if tokens.refresh_token.is_empty() {
        return Err("credentials cannot be refreshed".to_string());
    }

    let response = http
        .post(refresh_token_endpoint())
        .header(CONTENT_TYPE, "application/json")
        .json(&RefreshRequest {
            client_id: CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: tokens.refresh_token.clone(),
        })
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(if status == StatusCode::UNAUTHORIZED {
            "credentials expired".to_string()
        } else {
            format!("token refresh failed: {status}")
        });
    }

    let refreshed: RefreshResponse = serde_json::from_str(&body)
        .map_err(|error| format!("failed to decode refreshed credentials: {error}"))?;
    if let Some(id_token) = refreshed.id_token {
        tokens.id_token = parse_chatgpt_jwt_claims(&id_token)
            .map_err(|error| format!("failed to parse refreshed ID token: {error}"))?;
    }
    if let Some(access_token) = refreshed.access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refreshed.refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth.last_refresh = Some(Utc::now());

    Ok(())
}

async fn request_usage_payload(
    http: &reqwest::Client,
    auth: &AuthDotJson,
    chatgpt_base_url: Option<&str>,
) -> Result<RateLimitStatusPayload, UsageFetchError> {
    let Some(tokens) = auth.tokens.as_ref() else {
        return Err(UsageFetchError::Other(
            "token data is not available".to_string(),
        ));
    };

    let url = usage_request_url(chatgpt_base_url);
    let response = http
        .get(&url)
        .headers(usage_headers(auth, tokens))
        .send()
        .await
        .map_err(|error| UsageFetchError::Other(error.to_string()))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.text().await.unwrap_or_default();

    if status == StatusCode::UNAUTHORIZED {
        return Err(UsageFetchError::Unauthorized);
    }
    if !status.is_success() {
        return Err(UsageFetchError::Other(format!(
            "usage request failed: {status}; content-type={content_type}"
        )));
    }

    serde_json::from_str::<RateLimitStatusPayload>(&body).map_err(|error| {
        UsageFetchError::Other(format!(
            "failed to decode usage payload: {error}; content-type={content_type}"
        ))
    })
}

fn usage_headers(auth: &AuthDotJson, tokens: &TokenData) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(user_agent) = HeaderValue::from_str(&get_codex_user_agent()) {
        headers.insert(USER_AGENT, user_agent);
    }
    if let Ok(authorization) = HeaderValue::from_str(&format!("Bearer {}", tokens.access_token)) {
        headers.insert(AUTHORIZATION, authorization);
    }
    let account_id = auth
        .agent_identity
        .as_ref()
        .map(|record| record.account_id.as_str())
        .or(tokens.account_id.as_deref())
        .or(tokens.id_token.chatgpt_account_id.as_deref());
    if let Some(account_id) = account_id
        && let Ok(name) = HeaderName::from_bytes(b"ChatGPT-Account-Id")
        && let Ok(value) = HeaderValue::from_str(account_id)
    {
        headers.insert(name, value);
    }
    let is_fedramp = auth
        .agent_identity
        .as_ref()
        .is_some_and(|record| record.chatgpt_account_is_fedramp)
        || tokens.id_token.is_fedramp_account();
    if is_fedramp && let Ok(name) = HeaderName::from_bytes(b"X-OpenAI-Fedramp") {
        headers.insert(name, HeaderValue::from_static("true"));
    }
    headers
}

fn usage_request_url(chatgpt_base_url: Option<&str>) -> String {
    let mut base_url = chatgpt_base_url
        .unwrap_or("https://chatgpt.com")
        .trim_end_matches('/')
        .to_string();
    if (base_url.starts_with("https://chatgpt.com")
        || base_url.starts_with("https://chat.openai.com"))
        && !base_url.contains("/backend-api")
    {
        base_url = format!("{base_url}/backend-api");
    }

    if base_url.contains("/backend-api") {
        format!("{base_url}/wham/usage")
    } else {
        format!("{base_url}/api/codex/usage")
    }
}

fn refresh_token_endpoint() -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| REFRESH_TOKEN_URL.to_string())
}

fn rate_limit_snapshots_from_payload(payload: RateLimitStatusPayload) -> Vec<RateLimitSnapshot> {
    let plan_type = Some(map_backend_plan_type(payload.plan_type));
    let rate_limit_reached_type = payload
        .rate_limit_reached_type
        .flatten()
        .and_then(|details| map_rate_limit_reached_type(details.kind));

    let mut snapshots = vec![make_rate_limit_snapshot(
        Some("codex".to_string()),
        /*limit_name*/ None,
        payload.rate_limit.flatten().map(|details| *details),
        payload.credits.flatten().map(|details| *details),
        plan_type,
        rate_limit_reached_type,
    )];
    if let Some(additional) = payload.additional_rate_limits.flatten() {
        snapshots.extend(additional.into_iter().map(|details| {
            make_rate_limit_snapshot(
                Some(details.metered_feature),
                Some(details.limit_name),
                details.rate_limit.flatten().map(|rate_limit| *rate_limit),
                /*credits*/ None,
                plan_type,
                /*rate_limit_reached_type*/ None,
            )
        }));
    }
    snapshots
}

fn make_rate_limit_snapshot(
    limit_id: Option<String>,
    limit_name: Option<String>,
    rate_limit: Option<RateLimitStatusDetails>,
    credits: Option<CreditStatusDetails>,
    plan_type: Option<AccountPlanType>,
    rate_limit_reached_type: Option<RateLimitReachedType>,
) -> RateLimitSnapshot {
    let (primary, secondary) = match rate_limit {
        Some(details) => (
            map_rate_limit_window(details.primary_window),
            map_rate_limit_window(details.secondary_window),
        ),
        None => (None, None),
    };
    RateLimitSnapshot {
        limit_id,
        limit_name,
        primary,
        secondary,
        credits: map_credits(credits),
        plan_type,
        rate_limit_reached_type,
    }
}

fn map_rate_limit_reached_type(kind: BackendRateLimitReachedKind) -> Option<RateLimitReachedType> {
    match kind {
        BackendRateLimitReachedKind::RateLimitReached => {
            Some(RateLimitReachedType::RateLimitReached)
        }
        BackendRateLimitReachedKind::WorkspaceOwnerCreditsDepleted => {
            Some(RateLimitReachedType::WorkspaceOwnerCreditsDepleted)
        }
        BackendRateLimitReachedKind::WorkspaceMemberCreditsDepleted => {
            Some(RateLimitReachedType::WorkspaceMemberCreditsDepleted)
        }
        BackendRateLimitReachedKind::WorkspaceOwnerUsageLimitReached => {
            Some(RateLimitReachedType::WorkspaceOwnerUsageLimitReached)
        }
        BackendRateLimitReachedKind::WorkspaceMemberUsageLimitReached => {
            Some(RateLimitReachedType::WorkspaceMemberUsageLimitReached)
        }
        BackendRateLimitReachedKind::Unknown => None,
    }
}

fn map_rate_limit_window(
    window: Option<Option<Box<RateLimitWindowSnapshot>>>,
) -> Option<RateLimitWindow> {
    let snapshot = window.flatten().map(|details| *details)?;
    Some(RateLimitWindow {
        used_percent: f64::from(snapshot.used_percent),
        window_minutes: window_minutes_from_seconds(snapshot.limit_window_seconds),
        resets_at: Some(i64::from(snapshot.reset_at)),
    })
}

fn window_minutes_from_seconds(seconds: i32) -> Option<i64> {
    if seconds <= 0 {
        return None;
    }

    let seconds = i64::from(seconds);
    Some((seconds + 59) / 60)
}

fn map_credits(credits: Option<CreditStatusDetails>) -> Option<CreditsSnapshot> {
    let details = credits?;
    Some(CreditsSnapshot {
        has_credits: details.has_credits,
        unlimited: details.unlimited,
        balance: details.balance.flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use tempfile::tempdir;

    fn api_key_auth(api_key: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(ApiAuthMode::ApiKey),
            openai_api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
        }
    }

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        let id_token = fake_jwt(serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "pro"
            }
        }));
        let parsed_id_token =
            crate::token_data::parse_chatgpt_jwt_claims(&id_token).expect("valid fake jwt");

        AuthDotJson {
            auth_mode: Some(ApiAuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: parsed_id_token,
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
        }
    }

    fn fake_jwt(payload: serde_json::Value) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json"));
        let payload_b64 =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload json"));
        let signature_b64 = URL_SAFE_NO_PAD.encode(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    #[test]
    fn upsert_and_list_saved_accounts_tracks_active_account() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_a", "a@example.com"))
            .expect("save account a");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_b", "b@example.com"))
            .expect("save account b");

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].key, "chatgpt:acct_b");
        assert_eq!(accounts[0].label, "b@example.com");
        assert!(accounts[0].is_active);
        assert_eq!(accounts[1].key, "chatgpt:acct_a");
        assert!(!accounts[1].is_active);
    }

    #[test]
    fn switch_saved_account_marks_selected_account_active() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_a", "a@example.com"))
            .expect("save account a");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_b", "b@example.com"))
            .expect("save account b");

        let selected = switch_saved_account(temp.path(), "chatgpt:acct_a").expect("switch account");
        assert_eq!(
            selected.tokens.and_then(|tokens| tokens.account_id),
            Some("acct_a".to_string())
        );

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert_eq!(accounts[0].key, "chatgpt:acct_a");
        assert!(accounts[0].is_active);
        assert_eq!(accounts[1].key, "chatgpt:acct_b");
        assert!(!accounts[1].is_active);
    }

    #[test]
    fn remove_inactive_account_preserves_active_account() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_a", "a@example.com"))
            .expect("save account a");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_b", "b@example.com"))
            .expect("save account b");

        let result =
            remove_saved_account(temp.path(), "chatgpt:acct_a").expect("remove inactive account");
        assert_eq!(
            result,
            RemoveSavedAccountResultInternal::Inactive {
                removed: SavedAccountSummary {
                    key: "chatgpt:acct_a".to_string(),
                    label: "a@example.com".to_string(),
                    auth_mode: ApiAuthMode::Chatgpt,
                    is_active: false,
                },
            }
        );

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].key, "chatgpt:acct_b");
        assert!(accounts[0].is_active);
    }

    #[test]
    fn remove_active_account_switches_to_next_saved_account() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_a", "a@example.com"))
            .expect("save account a");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_b", "b@example.com"))
            .expect("save account b");

        let result =
            remove_saved_account(temp.path(), "chatgpt:acct_b").expect("remove active account");
        match result {
            RemoveSavedAccountResultInternal::ActiveSwitched {
                removed,
                replacement,
                replacement_auth,
            } => {
                assert_eq!(removed.key, "chatgpt:acct_b");
                assert_eq!(replacement.key, "chatgpt:acct_a");
                assert_eq!(
                    replacement_auth.tokens.and_then(|tokens| tokens.account_id),
                    Some("acct_a".to_string())
                );
            }
            other => panic!("unexpected removal result: {other:?}"),
        }

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].key, "chatgpt:acct_a");
        assert!(accounts[0].is_active);
    }

    #[test]
    fn remove_last_active_account_clears_active_key() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &chatgpt_auth("acct_a", "a@example.com"))
            .expect("save account a");

        let result =
            remove_saved_account(temp.path(), "chatgpt:acct_a").expect("remove active account");
        assert_eq!(
            result,
            RemoveSavedAccountResultInternal::LastActive {
                removed: SavedAccountSummary {
                    key: "chatgpt:acct_a".to_string(),
                    label: "a@example.com".to_string(),
                    auth_mode: ApiAuthMode::Chatgpt,
                    is_active: true,
                },
            }
        );

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert!(accounts.is_empty());
    }

    #[test]
    fn upsert_api_key_account_uses_stable_api_prefix() {
        let temp = tempdir().expect("temp dir");
        upsert_saved_account(temp.path(), &api_key_auth("sk-test-account-key"))
            .expect("save api key account");

        let accounts = list_saved_accounts(temp.path()).expect("list accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].auth_mode, ApiAuthMode::ApiKey);
        assert!(accounts[0].key.starts_with("api:"));
        assert_eq!(accounts[0].label, "API key (...-key)");
    }
}
