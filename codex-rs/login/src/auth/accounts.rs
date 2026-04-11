use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::storage::AuthDotJson;

const ACCOUNTS_FILE_NAME: &str = "accounts.json";
const ACCOUNTS_FILE_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAccountSummary {
    pub key: String,
    pub label: String,
    pub auth_mode: ApiAuthMode,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

const fn saved_accounts_file_version() -> u8 {
    ACCOUNTS_FILE_VERSION
}

pub(super) fn upsert_saved_account(codex_home: &Path, auth: &AuthDotJson) -> std::io::Result<()> {
    let auth_mode = resolved_auth_mode(auth);
    if auth_mode == ApiAuthMode::ChatgptAuthTokens {
        // External ChatGPT auth tokens are managed out-of-band and should not be persisted.
        return Ok(());
    }

    let (key, label) = account_identity(auth, auth_mode)?;
    let mut saved = read_saved_accounts(codex_home)?;

    let entry = SavedAccountEntry {
        key: key.clone(),
        label,
        auth_mode,
        auth: auth.clone(),
        updated_at: Utc::now(),
    };

    saved.accounts.retain(|existing| existing.key != key);
    saved.accounts.insert(0, entry);
    saved.active_account_key = Some(key);

    write_saved_accounts(codex_home, &saved)
}

pub fn list_saved_accounts(codex_home: &Path) -> std::io::Result<Vec<SavedAccountSummary>> {
    let mut saved = read_saved_accounts(codex_home)?;
    saved.accounts.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.label.cmp(&right.label))
    });

    let active_key = saved.active_account_key.as_deref();
    Ok(saved
        .accounts
        .into_iter()
        .map(|entry| SavedAccountSummary {
            key: entry.key.clone(),
            label: entry.label,
            auth_mode: entry.auth_mode,
            is_active: active_key == Some(entry.key.as_str()),
        })
        .collect())
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
    saved.accounts.insert(0, entry);
    write_saved_accounts(codex_home, &saved)?;
    Ok(auth)
}

fn resolved_auth_mode(auth: &AuthDotJson) -> ApiAuthMode {
    if let Some(mode) = auth.auth_mode {
        return mode;
    }
    if auth.openai_api_key.is_some() {
        return ApiAuthMode::ApiKey;
    }
    ApiAuthMode::Chatgpt
}

fn account_identity(
    auth: &AuthDotJson,
    auth_mode: ApiAuthMode,
) -> std::io::Result<(String, String)> {
    if auth_mode == ApiAuthMode::ApiKey {
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
        return Ok((
            format!("api:{}", short_hash(api_key.as_bytes())),
            format!("API key (...{suffix})"),
        ));
    }

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
    if parsed
        .active_account_key
        .as_ref()
        .is_some_and(|active| !parsed.accounts.iter().any(|entry| &entry.key == active))
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_data::TokenData;
    use base64::Engine;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use tempfile::tempdir;

    fn api_key_auth(api_key: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(ApiAuthMode::ApiKey),
            openai_api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
        }
    }

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        let id_token = fake_jwt(serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id
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

        fn b64url_no_pad(bytes: &[u8]) -> String {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).expect("header json"));
        let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).expect("payload json"));
        let signature_b64 = b64url_no_pad(b"sig");
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
