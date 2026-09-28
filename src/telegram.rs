use crate::alert::{AlertSink, DeliveryChannel};
use crate::settings::TrackerConfig;
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const MAX_TELEGRAM_TEXT_CHARS: usize = 4_096;

/// Small direct Rust client for Telegram's HTTPS Bot API.
///
/// The bot token is kept private and is never included in formatted request
/// errors. This client does not receive updates during normal polling.
pub struct TelegramClient {
    http: Client,
    token: String,
}

impl TelegramClient {
    /// Create a client from a token issued by Telegram's `@BotFather`.
    ///
    /// # Errors
    /// Returns an error if the token does not have the expected `id:secret`
    /// shape or an HTTPS client cannot be created.
    pub fn new(token: &str) -> Result<Self> {
        let token = token.trim();
        let Some((bot_id, secret)) = token.split_once(':') else {
            bail!("TELEGRAM_BOT_TOKEN must be the token issued by @BotFather")
        };
        if bot_id.is_empty()
            || !bot_id.bytes().all(|byte| byte.is_ascii_digit())
            || secret.is_empty()
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            bail!("TELEGRAM_BOT_TOKEN must be the token issued by @BotFather")
        }

        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(55))
            .build()
            .map_err(|_| anyhow::anyhow!("could not create the Telegram HTTPS client"))?;
        Ok(Self {
            http,
            token: token.to_owned(),
        })
    }

    /// Verify the bot token and return the bot's username, if present.
    ///
    /// # Errors
    /// Returns an error when Telegram cannot validate the token or the API is
    /// unavailable.
    pub fn get_bot_username(&self) -> Result<Option<String>> {
        let bot: TelegramBot = self.get("getMe")?;
        Ok(bot.username)
    }

    /// Look for private chats with recent messages to this bot.
    ///
    /// This is intended for the one-time chat-discovery setup flow. The tracker
    /// does not poll incoming messages while it runs. Telegram's `getUpdates`
    /// method cannot be used while the bot has a webhook configured.
    ///
    /// # Errors
    /// Returns an error if the Bot API request fails or the token is invalid.
    pub fn discover_private_chat_ids(&self, wait_seconds: u64) -> Result<Vec<i64>> {
        let request = GetUpdatesRequest {
            timeout: wait_seconds.min(50),
            allowed_updates: ["message"],
        };
        let updates: Vec<TelegramUpdate> = self.post("getUpdates", &request)?;
        let mut chat_ids = BTreeSet::new();
        for update in updates {
            if let Some(message) = update.message
                && message.chat.chat_type == "private"
            {
                chat_ids.insert(message.chat.id);
            }
        }
        Ok(chat_ids.into_iter().collect())
    }

    fn endpoint(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    fn get<T: DeserializeOwned>(&self, method: &str) -> Result<T> {
        let response =
            self.http.get(self.endpoint(method)).send().map_err(|_| {
                anyhow::anyhow!("Telegram Bot API request failed; check connectivity")
            })?;
        decode_response(response)
    }

    fn post<T: DeserializeOwned, B: Serialize>(&self, method: &str, body: &B) -> Result<T> {
        let response = self
            .http
            .post(self.endpoint(method))
            .json(body)
            .send()
            .map_err(|_| anyhow::anyhow!("Telegram Bot API request failed; check connectivity"))?;
        decode_response(response)
    }
}

/// Telegram destination configured for NSE alerts.
pub struct TelegramSender {
    client: TelegramClient,
    chat_id: i64,
    last_send: Mutex<Option<Instant>>,
}

impl TelegramSender {
    /// Build a sender from `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`.
    ///
    /// # Errors
    /// Returns an error when either setting is absent, the chat ID is not a
    /// signed numeric Telegram chat identifier, or the token is malformed.
    pub fn connect(config: &TrackerConfig) -> Result<Self> {
        let token = config
            .telegram_bot_token
            .as_deref()
            .context("configure a Telegram bot token before using Telegram")?;
        let chat_id = config
            .telegram_chat_id
            .as_deref()
            .context("configure a Telegram destination chat before using Telegram")?
            .parse::<i64>()
            .context("TELEGRAM_CHAT_ID must be a numeric Telegram chat ID")?;
        Ok(Self {
            client: TelegramClient::new(token)?,
            chat_id,
            last_send: Mutex::new(None),
        })
    }

    /// Send one text message to the configured chat.
    ///
    /// Telegram accepts at most 4,096 characters for a text message.
    ///
    /// # Errors
    /// Returns an error when the message is too long, the bot token is invalid,
    /// or Telegram rejects the request.
    pub fn send_text(&self, message: &str) -> Result<()> {
        if message.chars().count() > MAX_TELEGRAM_TEXT_CHARS {
            bail!("Telegram message exceeds the 4096-character Bot API limit")
        }
        let mut last_send = self
            .last_send
            .lock()
            .map_err(|_| anyhow::anyhow!("Telegram send-rate lock was poisoned"))?;
        if let Some(previous) = *last_send {
            let minimum_gap = Duration::from_millis(1_100);
            let elapsed = previous.elapsed();
            if elapsed < minimum_gap {
                std::thread::sleep(minimum_gap - elapsed);
            }
        }
        *last_send = Some(Instant::now());
        let request = SendMessageRequest {
            chat_id: self.chat_id,
            text: message,
            link_preview_options: LinkPreviewOptions { is_disabled: true },
        };
        let _: serde_json::Value = self.client.post("sendMessage", &request)?;
        Ok(())
    }
}

impl AlertSink for TelegramSender {
    fn channel(&self) -> DeliveryChannel {
        DeliveryChannel::Telegram
    }

    fn send_text(&self, message: &str) -> Result<()> {
        TelegramSender::send_text(self, message)
    }
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
    link_preview_options: LinkPreviewOptions,
}

#[derive(Serialize)]
struct LinkPreviewOptions {
    is_disabled: bool,
}

#[derive(Serialize)]
struct GetUpdatesRequest {
    timeout: u64,
    allowed_updates: [&'static str; 1],
}

#[derive(Deserialize)]
struct TelegramBot {
    username: Option<String>,
}

#[derive(Deserialize)]
struct TelegramUpdate {
    message: Option<TelegramMessage>,
}

#[derive(Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
}

#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

fn decode_response<T: DeserializeOwned>(response: Response) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|_| anyhow::anyhow!("could not read Telegram Bot API response"))?;
    let response: TelegramResponse<T> = serde_json::from_str(&body)
        .map_err(|_| anyhow::anyhow!("Telegram returned an invalid Bot API response"))?;
    if !status.is_success() || !response.ok {
        let description = response
            .description
            .unwrap_or_else(|| format!("HTTP status {status}"));
        bail!("Telegram Bot API error: {description}")
    }
    response
        .result
        .context("Telegram Bot API response did not contain a result")
}
