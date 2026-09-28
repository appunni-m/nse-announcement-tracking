//! Track NSE corporate announcements, summarize PDF text through the installed
//! Codex CLI, and deliver alerts through WhatsApp Web or the Telegram Bot API.
//!
//! The crate exposes [`Tracker`] for application integration. The companion
//! `nse-tracker-widget` binary is a small desktop interface built on this API.
//! The tracker stores its watchlist in `stocks.json` and delivery state in
//! SQLite under the configured state directory.
//!
//! # Quick start
//!
//! ```no_run
//! use nse_tracker::{MarketApiType, Tracker, TrackerConfig};
//!
//! let config = TrackerConfig::from_dir(".")?;
//! let tracker = Tracker::new(config)?;
//! tracker.add_stock("ALPEXSOLAR", MarketApiType::Sme)?;
//! let result = tracker.poll_once(&[], false)?;
//! println!("checked {} symbols", result.symbols_checked);
//! # Ok::<(), anyhow::Error>(())
//! ```

mod alert;
mod codex;
mod config;
mod nse;
mod settings;
mod store;
mod telegram;
mod whatsapp;
mod worker;

pub use alert::{AlertSink, DeliveryChannel};
pub use config::{MarketApiType, StockEntry};
pub use settings::TrackerConfig;
pub use store::AnnouncementRecord;
pub use telegram::{TelegramClient, TelegramSender};
pub use whatsapp::{WhatsAppEvent, WhatsAppSender};
pub use worker::{PollSummary, Tracker, read_watchlist, validate_symbol};
