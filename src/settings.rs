use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub(crate) fn ensure_private_state_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("could not create state directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .with_context(|| format!("could not inspect state directory {}", path.display()))?
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("could not restrict permissions on {}", path.display()))?;
    }
    Ok(())
}

/// Paths and runtime settings shared by the library, CLI, and desktop widget.
#[derive(Clone)]
pub struct TrackerConfig {
    /// Project directory containing `.env` and `stocks.json`.
    pub root_dir: PathBuf,
    /// Editable JSON file holding the current NSE watchlist.
    pub stocks_file: PathBuf,
    /// Directory for the announcement database and WhatsApp linked-device state.
    pub state_dir: PathBuf,
    /// SQLite database containing discovered announcements and delivery state.
    pub database_file: PathBuf,
    /// SQLite database containing the WhatsApp linked-device session.
    pub whatsapp_database_file: PathBuf,
    /// Phone number used to pair this client, with country code and digits only.
    pub pair_phone: Option<String>,
    /// Alert destination; defaults to the paired phone number for self-chat.
    pub alert_phone: Option<String>,
    /// Telegram bot token issued by `@BotFather`.
    pub telegram_bot_token: Option<String>,
    /// Numeric Telegram chat ID that receives alerts.
    pub telegram_chat_id: Option<String>,
    /// Seconds between polling cycles. Defaults to 60 seconds.
    pub poll_interval_seconds: u64,
    /// Calendar days of NSE filings requested on each poll.
    pub lookback_days: i64,
    /// Delay between individual NSE symbol requests in milliseconds.
    pub request_delay_ms: u64,
    /// Maximum number of extracted PDF text characters included in an alert.
    pub pdf_text_limit_chars: usize,
    /// Maximum PDF download size in bytes.
    pub max_pdf_bytes: u64,
    /// Whether extracted announcement PDF text is summarized using Codex CLI.
    pub codex_pdf_analysis: bool,
    /// Optional path or command name for the Codex CLI executable.
    pub codex_cli_path: Option<PathBuf>,
}

impl TrackerConfig {
    /// Load settings from process environment variables and an optional `.env` file.
    ///
    /// Process environment variables take precedence over values in `.env`.
    /// Paths are resolved against `root_dir` so the CLI and widget can share state.
    ///
    /// # Errors
    /// Returns an error for malformed `.env` values or settings outside their
    /// supported ranges.
    pub fn from_dir(root_dir: impl AsRef<Path>) -> Result<Self> {
        let root_dir = root_dir.as_ref().to_path_buf();
        let file_values = read_dotenv(&root_dir.join(".env"))?;
        let value = |name: &str| -> Option<String> {
            std::env::var(name)
                .ok()
                .or_else(|| file_values.get(name).cloned())
        };

        let state_dir = root_dir.join(".nse-tracker");
        let pair_phone = value("WHATSAPP_PAIR_PHONE")
            .filter(|item| !item.trim().is_empty())
            .map(|phone| normalize_phone(&phone))
            .transpose()?;
        let alert_phone = value("WHATSAPP_ALERT_PHONE")
            .filter(|item| !item.trim().is_empty())
            .map(|phone| normalize_phone(&phone))
            .transpose()?
            .or_else(|| pair_phone.clone());
        let telegram_bot_token = value("TELEGRAM_BOT_TOKEN")
            .filter(|item| !item.trim().is_empty())
            .map(|token| token.trim().to_owned());
        let telegram_chat_id = value("TELEGRAM_CHAT_ID")
            .filter(|item| !item.trim().is_empty())
            .map(|chat_id| chat_id.trim().to_owned());

        Ok(Self {
            root_dir: root_dir.clone(),
            stocks_file: root_dir.join("stocks.json"),
            state_dir: state_dir.clone(),
            database_file: state_dir.join("state.sqlite3"),
            whatsapp_database_file: state_dir.join("whatsapp.sqlite3"),
            pair_phone,
            alert_phone,
            telegram_bot_token,
            telegram_chat_id,
            poll_interval_seconds: setting_u64(
                &value,
                "NSE_POLL_INTERVAL_SECONDS",
                60,
                60,
                86_400,
            )?,
            lookback_days: setting_u64(&value, "NSE_LOOKBACK_DAYS", 7, 1, 30)? as i64,
            request_delay_ms: setting_u64(&value, "NSE_REQUEST_DELAY_MS", 300, 0, 60_000)?,
            pdf_text_limit_chars: setting_u64(&value, "PDF_TEXT_LIMIT_CHARS", 2_200, 100, 10_000)?
                as usize,
            max_pdf_bytes: setting_u64(&value, "MAX_PDF_BYTES", 25_000_000, 100_000, 100_000_000)?,
            codex_pdf_analysis: setting_bool(&value, "CODEX_PDF_ANALYSIS", true)?,
            codex_cli_path: value("CODEX_CLI_PATH")
                .filter(|path| !path.trim().is_empty())
                .map(|path| PathBuf::from(path.trim())),
        })
    }
}

impl fmt::Debug for TrackerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrackerConfig")
            .field("root_dir", &self.root_dir)
            .field("stocks_file", &self.stocks_file)
            .field("state_dir", &self.state_dir)
            .field("database_file", &self.database_file)
            .field("whatsapp_database_file", &self.whatsapp_database_file)
            .field(
                "pair_phone",
                &self.pair_phone.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "alert_phone",
                &self.alert_phone.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "telegram_bot_token",
                &self.telegram_bot_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "telegram_chat_id",
                &self.telegram_chat_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("poll_interval_seconds", &self.poll_interval_seconds)
            .field("lookback_days", &self.lookback_days)
            .field("request_delay_ms", &self.request_delay_ms)
            .field("pdf_text_limit_chars", &self.pdf_text_limit_chars)
            .field("max_pdf_bytes", &self.max_pdf_bytes)
            .field("codex_pdf_analysis", &self.codex_pdf_analysis)
            .field(
                "codex_cli_path",
                &self.codex_cli_path.as_ref().map(|_| "[CONFIGURED]"),
            )
            .finish()
    }
}

fn read_dotenv(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let mut values = HashMap::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            bail!(
                "invalid .env entry on line {}: expected KEY=value",
                index + 1
            );
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            bail!("invalid .env key on line {}", index + 1);
        }
        let value = raw_value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .to_owned();
        values.insert(key.to_owned(), value);
    }
    Ok(values)
}

fn setting_u64(
    value: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64> {
    let parsed = value(name)
        .unwrap_or_else(|| default.to_string())
        .parse::<u64>()
        .with_context(|| format!("{name} must be an integer"))?;
    if !(min..=max).contains(&parsed) {
        bail!("{name} must be between {min} and {max}");
    }
    Ok(parsed)
}

fn setting_bool(
    value: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: bool,
) -> Result<bool> {
    value(name)
        .unwrap_or_else(|| default.to_string())
        .parse::<bool>()
        .with_context(|| format!("{name} must be true or false"))
}

fn normalize_phone(raw: &str) -> Result<String> {
    let phone: String = raw
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if !(8..=15).contains(&phone.len()) {
        bail!("WhatsApp phone numbers must include the country code and contain 8 to 15 digits");
    }
    Ok(phone)
}
