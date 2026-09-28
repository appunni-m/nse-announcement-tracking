use crate::config::StockEntry;
use anyhow::{Context, Result, bail};
use chrono::{Duration as ChronoDuration, FixedOffset, Utc};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, REFERER};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde::Deserializer;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

const NSE_FILINGS_PAGE: &str =
    "https://www.nseindia.com/companies-listing/corporate-filings-announcements";
const NSE_API: &str = "https://www.nseindia.com/api/NextApi/apiClient/GetQuoteApi";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Announcement {
    #[serde(default, deserialize_with = "null_to_default")]
    pub desc: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub dt: String,
    #[serde(default, rename = "attchmntFile", deserialize_with = "empty_to_none")]
    pub attachment_file: Option<String>,
    #[serde(default, rename = "sm_name", deserialize_with = "empty_to_none")]
    pub company_name: Option<String>,
    #[serde(default, rename = "attchmntText", deserialize_with = "empty_to_none")]
    pub attachment_text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_identifier")]
    pub seq_id: String,
}

fn null_to_default<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn empty_to_none<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|value| !value.trim().is_empty()))
}

fn deserialize_identifier<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(value) => Ok(value),
        Value::Number(value) => Ok(value.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(serde::de::Error::custom(
            "expected an NSE sequence ID string or number",
        )),
    }
}

pub(crate) struct NseClient {
    client: Client,
    max_pdf_bytes: u64,
}

impl NseClient {
    pub fn new(max_pdf_bytes: u64) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if is_nse_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .timeout(Duration::from_secs(25))
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")
            .build()
            .context("could not build the NSE HTTP client")?;
        Ok(Self {
            client,
            max_pdf_bytes,
        })
    }

    pub fn warm_up(&self) -> Result<()> {
        let response = self
            .client
            .get(NSE_FILINGS_PAGE)
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            )
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-site", "none")
            .send()
            .context("could not reach NSE corporate filings page")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!(
                "NSE corporate filings page returned HTTP {status}: {}",
                short_text(&body, 500)
            );
        }
        Ok(())
    }

    pub fn announcements(
        &self,
        stock: &StockEntry,
        lookback_days: i64,
    ) -> Result<Vec<Announcement>> {
        let india_offset = FixedOffset::east_opt(5 * 60 * 60 + 30 * 60)
            .context("invalid India time zone offset")?;
        let today = Utc::now().with_timezone(&india_offset).date_naive();
        let from = today
            .checked_sub_signed(ChronoDuration::days(lookback_days))
            .context("could not calculate NSE lookback date")?;
        let mut url = Url::parse(NSE_API)?;
        url.query_pairs_mut()
            .append_pair("functionName", "getCorporateAnnouncement")
            .append_pair("symbol", &stock.symbol)
            .append_pair("marketApiType", stock.market_api_type.as_str())
            .append_pair("subject", "")
            .append_pair("fromDate", &from.format("%d-%m-%Y").to_string())
            .append_pair("toDate", &today.format("%d-%m-%Y").to_string());

        let response = self.get_api_response(&url)?;
        let status = response.status();
        let body = response
            .text()
            .context("could not read NSE announcement response")?;
        if !status.is_success() {
            bail!("NSE returned HTTP {status}: {}", short_text(&body, 500));
        }

        let value: Value = serde_json::from_str(&body).context("NSE returned invalid JSON")?;
        let Some(items) = value.as_array() else {
            bail!(
                "NSE returned an API error instead of an announcement list: {}",
                short_text(&body, 700)
            );
        };
        serde_json::from_value(Value::Array(items.clone()))
            .with_context(|| format!("could not parse announcement fields for {}", stock.symbol))
    }

    fn get_api_response(&self, url: &Url) -> Result<Response> {
        let response = self
            .api_request(url)
            .send()
            .context("request to NSE announcement API failed")?;

        if response.status() == StatusCode::FORBIDDEN {
            self.warm_up()
                .context("NSE rejected the API request and filings-page refresh failed")?;
            return self
                .api_request(url)
                .send()
                .context("retry to NSE announcement API failed");
        }
        Ok(response)
    }

    fn api_request(&self, url: &Url) -> reqwest::blocking::RequestBuilder {
        self.client
            .get(url.clone())
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header("origin", "https://www.nseindia.com")
            .header(REFERER, NSE_FILINGS_PAGE)
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-origin")
            .header("cache-control", "no-cache")
    }

    pub fn extract_attachment_text(&self, attachment_url: &str) -> Result<String> {
        let url = Url::parse(attachment_url).context("NSE supplied an invalid PDF URL")?;
        let Some(host) = url.host_str() else {
            bail!("NSE PDF URL has no hostname");
        };
        if url.scheme() != "https" || !(host == "nseindia.com" || host.ends_with(".nseindia.com")) {
            bail!("refusing to download an attachment outside nseindia.com");
        }

        let response = self
            .client
            .get(url)
            .send()
            .context("could not download NSE PDF")?;
        let response = response
            .error_for_status()
            .context("NSE PDF download returned an error")?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_pdf_bytes)
        {
            bail!(
                "NSE PDF is larger than the configured {} byte limit",
                self.max_pdf_bytes
            );
        }
        let mut bytes = Vec::new();
        response
            .take(self.max_pdf_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .context("could not read NSE PDF bytes")?;
        if bytes.len() as u64 > self.max_pdf_bytes {
            bail!(
                "NSE PDF is larger than the configured {} byte limit",
                self.max_pdf_bytes
            );
        }
        if !bytes.starts_with(b"%PDF") {
            bail!("NSE attachment did not contain PDF data");
        }
        let text = pdf_extract::extract_text_from_mem(&bytes)
            .context("could not extract text from NSE PDF")?;
        Ok(normalize_whitespace(&text))
    }
}

fn is_nse_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "nseindia.com" || host.ends_with(".nseindia.com"))
}

pub(crate) fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn clip_chars(input: &str, limit: usize) -> String {
    let mut chars = input.chars();
    let clipped: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{clipped}… [truncated]")
    } else {
        clipped
    }
}

fn short_text(input: &str, limit: usize) -> String {
    clip_chars(&normalize_whitespace(input), limit)
}
