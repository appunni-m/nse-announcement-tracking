use nse_tracker::TrackerConfig;
use std::path::Path;

const SERVICE: &str = "com.nse.tracker.messaging";
const DISABLED_MARKER: &str = "nse-tracker-disabled-v1";
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

pub const WHATSAPP_PAIR_PHONE: &str = "whatsapp-pair-phone";
pub const WHATSAPP_ALERT_PHONE: &str = "whatsapp-alert-phone";
pub const TELEGRAM_BOT_TOKEN: &str = "telegram-bot-token";
pub const TELEGRAM_CHAT_ID: &str = "telegram-chat-id";

/// Load saved macOS Keychain credentials and migrate any legacy `.env` values.
///
/// The Keychain wins when an entry exists. Legacy values are copied into the
/// Keychain once, then removed from `.env` so secrets are not left in plaintext.
/// The returned flag indicates that at least one legacy value was migrated.
pub fn load_into_config(config: &mut TrackerConfig) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let saved_pair = read(WHATSAPP_PAIR_PHONE)?;
        let saved_alert = read(WHATSAPP_ALERT_PHONE)?;
        let saved_token = read(TELEGRAM_BOT_TOKEN)?;
        let saved_chat = read(TELEGRAM_CHAT_ID)?;

        let pair_was_saved = saved_pair.is_some();
        let alert_was_saved = saved_alert.is_some();
        let token_was_saved = saved_token.is_some();
        let chat_was_saved = saved_chat.is_some();
        let (pair_phone, pair_migrated) =
            resolve(saved_pair, config.pair_phone.clone(), WHATSAPP_PAIR_PHONE)?;
        let alert_legacy = if pair_was_saved {
            pair_phone.clone()
        } else {
            config.alert_phone.clone()
        };
        let (alert_phone, alert_migrated) =
            resolve(saved_alert, alert_legacy, WHATSAPP_ALERT_PHONE)?;
        let (bot_token, token_migrated) = resolve(
            saved_token,
            config.telegram_bot_token.clone(),
            TELEGRAM_BOT_TOKEN,
        )?;
        let chat_legacy = if token_was_saved {
            None
        } else {
            config.telegram_chat_id.clone()
        };
        let (chat_id, chat_migrated) = resolve(saved_chat, chat_legacy, TELEGRAM_CHAT_ID)?;

        config.pair_phone = pair_phone;
        config.alert_phone = alert_phone;
        config.telegram_bot_token = bot_token;
        config.telegram_chat_id = chat_id;

        let changed = pair_migrated || alert_migrated || token_migrated || chat_migrated;
        let has_saved_credentials =
            pair_was_saved || alert_was_saved || token_was_saved || chat_was_saved;
        if changed || has_saved_credentials {
            redact_legacy_file(&config.root_dir)?;
        }
        Ok(changed)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        Ok(false)
    }
}

/// Save a value in the macOS Keychain without writing it to a settings file.
pub fn save(account: &str, value: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(SERVICE, account, value.as_bytes())
            .map_err(|error| format!("macOS Keychain save failed (status {})", error.code()))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (account, value);
        Err("messaging credentials can currently be saved only on macOS".to_owned())
    }
}

fn read(account: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        use security_framework::passwords::{PasswordOptions, generic_password};

        match generic_password(PasswordOptions::new_generic_password(SERVICE, account)) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "a saved Keychain value is not valid UTF-8".to_owned()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(format!(
                "macOS Keychain read failed (status {})",
                error.code()
            )),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = account;
        Ok(None)
    }
}

fn resolve(
    saved: Option<String>,
    legacy: Option<String>,
    account: &str,
) -> Result<(Option<String>, bool), String> {
    if let Some(value) = saved {
        return Ok((decode_marker(value), false));
    }
    if let Some(value) = legacy {
        save(account, &value)?;
        return Ok((Some(value), true));
    }
    Ok((None, false))
}

fn decode_marker(value: String) -> Option<String> {
    (value != DISABLED_MARKER).then_some(value)
}

pub fn disable_marker() -> &'static str {
    DISABLED_MARKER
}

pub fn redact_legacy_file(root: &Path) -> Result<(), String> {
    let path = root.join(".env");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("could not read legacy messaging settings".to_owned()),
    };

    let mut redacted = String::with_capacity(contents.len());
    let mut changed = false;
    for line in contents.lines() {
        let is_credential = line
            .split_once('=')
            .is_some_and(|(key, _)| is_credential_key(key.trim()));
        if is_credential {
            let key = line
                .split_once('=')
                .map(|(key, _)| key.trim())
                .unwrap_or("");
            redacted.push_str(key);
            redacted.push_str("=\n");
            changed = true;
        } else {
            redacted.push_str(line);
            redacted.push('\n');
        }
    }
    if !changed {
        return Ok(());
    }

    let temporary = root.join(format!(".env.keychain-{}.tmp", std::process::id()));
    write_private_file(&temporary, redacted.as_bytes())
        .map_err(|_| "could not securely update legacy messaging settings".to_owned())?;
    if std::fs::rename(&temporary, &path).is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err("could not replace legacy messaging settings".to_owned());
    }
    Ok(())
}

fn is_credential_key(key: &str) -> bool {
    matches!(
        key,
        "WHATSAPP_PAIR_PHONE" | "WHATSAPP_ALERT_PHONE" | "TELEGRAM_BOT_TOKEN" | "TELEGRAM_CHAT_ID"
    )
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    use std::io::Write;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}
