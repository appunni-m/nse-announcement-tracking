use crate::alert::{AlertSink, DeliveryChannel};
use crate::codex::CodexQueue;
use crate::config::{MarketApiType, StockEntry, StockList, normalize_symbol};
use crate::nse::{Announcement, NseClient, clip_chars};
use crate::settings::{TrackerConfig, ensure_private_state_dir};
use crate::store::{AnnouncementRecord, PendingAnnouncement, Store};
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_PDF_ATTEMPTS: u32 = 3;
const MAX_ALERT_MESSAGE_CHARS: usize = 3_900;

/// Library facade for managing a watchlist, polling NSE, and delivering alerts.
///
/// Polling performs blocking HTTP and PDF extraction work. Codex analysis runs
/// on a separate queue, so alerts remain pending until analysis finishes. Run
/// [`Tracker::poll_once`] on a worker thread in graphical applications.
pub struct Tracker {
    config: TrackerConfig,
    nse: NseClient,
    store: Arc<Mutex<Store>>,
    codex_queue: Option<CodexQueue>,
    warmed_up: Mutex<bool>,
}

impl Tracker {
    /// Create the tracker, its state directory, default watchlist, and SQLite store.
    ///
    /// A missing `stocks.json` is initialized with `ALPEXSOLAR` as an SME symbol.
    ///
    /// # Errors
    /// Returns an error if the state directory, watchlist, database, HTTP
    /// client, or enabled Codex worker thread cannot be initialized.
    pub fn new(config: TrackerConfig) -> Result<Self> {
        ensure_private_state_dir(&config.state_dir)?;
        let stocks = StockList::load_or_default(&config.stocks_file)?;
        if !config.stocks_file.exists() {
            stocks.save(&config.stocks_file)?;
        }
        let nse = NseClient::new(config.max_pdf_bytes)?;
        let store = Arc::new(Mutex::new(Store::open(&config.database_file)?));
        let codex_queue = if config.codex_pdf_analysis {
            Some(CodexQueue::new(
                Arc::clone(&store),
                config.codex_cli_path.clone(),
            )?)
        } else {
            None
        };
        Ok(Self {
            config,
            nse,
            store,
            codex_queue,
            warmed_up: Mutex::new(false),
        })
    }

    /// Return the settings used by this tracker.
    pub fn config(&self) -> &TrackerConfig {
        &self.config
    }

    /// Return the current watchlist, re-reading `stocks.json` from disk.
    ///
    /// Re-reading means manual edits are picked up on the next poll.
    pub fn stock_list(&self) -> Result<Vec<StockEntry>> {
        Ok(StockList::load_or_default(&self.config.stocks_file)?.entries)
    }

    /// Add a symbol or update its market API route.
    ///
    /// Symbols are normalized to uppercase. This writes `stocks.json` using a
    /// temporary file followed by an atomic rename.
    pub fn add_stock(&self, symbol: &str, market_api_type: MarketApiType) -> Result<()> {
        let symbol = normalize_symbol(symbol)?;
        let mut stocks = StockList::load_or_default(&self.config.stocks_file)?;
        if let Some(existing) = stocks
            .entries
            .iter_mut()
            .find(|entry| entry.symbol == symbol)
        {
            existing.market_api_type = market_api_type;
        } else {
            stocks.entries.push(StockEntry {
                symbol,
                market_api_type,
            });
            stocks
                .entries
                .sort_by(|left, right| left.symbol.cmp(&right.symbol));
        }
        stocks.save(&self.config.stocks_file)
    }

    /// Stop tracking a symbol. Returns `true` when it was present in the list.
    pub fn remove_stock(&self, symbol: &str) -> Result<bool> {
        let symbol = normalize_symbol(symbol)?;
        let mut stocks = StockList::load_or_default(&self.config.stocks_file)?;
        let before = stocks.entries.len();
        stocks.entries.retain(|entry| entry.symbol != symbol);
        if before == stocks.entries.len() {
            return Ok(false);
        }
        stocks.save(&self.config.stocks_file)?;
        Ok(true)
    }

    /// Return recent announcements, newest first.
    pub fn recent_announcements(&self, limit: usize) -> Result<Vec<AnnouncementRecord>> {
        self.store
            .lock()
            .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
            .recent_announcements(limit)
    }

    /// Poll each configured symbol once, save new filings, and send ready alerts.
    ///
    /// On the first poll for a symbol, existing filings in the configured
    /// lookback window are stored as a baseline and are not sent unless
    /// `send_existing` is `true`. Pass an empty `senders` slice to collect
    /// announcements without delivery; pending messages stay queued in SQLite.
    /// Codex analysis is scheduled in the background, and a filing with text to
    /// analyze remains pending until its summary or analysis error is saved.
    ///
    /// # Errors
    /// Returns an error when the NSE warm-up request or local database operation
    /// fails. Per-symbol NSE failures are included in [`PollSummary::errors`].
    pub fn poll_once(
        &self,
        senders: &[&dyn AlertSink],
        send_existing: bool,
    ) -> Result<PollSummary> {
        let stocks = self.stock_list()?;
        let mut summary = PollSummary {
            symbols_checked: stocks.len(),
            ..PollSummary::default()
        };

        if !stocks.is_empty() {
            let mut warmed = self
                .warmed_up
                .lock()
                .map_err(|_| anyhow::anyhow!("NSE warm-up lock was poisoned"))?;
            if !*warmed {
                self.nse.warm_up()?;
                *warmed = true;
            }
            drop(warmed);
        }

        for (index, stock) in stocks.iter().enumerate() {
            let result = self.nse.announcements(stock, self.config.lookback_days);
            match result {
                Ok(announcements) => {
                    self.record_poll_result(stock, &announcements, send_existing, &mut summary)?;
                }
                Err(error) => summary.errors.push(format!("{}: {error:#}", stock.symbol)),
            }
            if index + 1 < stocks.len() && self.config.request_delay_ms > 0 {
                thread::sleep(Duration::from_millis(self.config.request_delay_ms));
            }
        }

        self.deliver_pending(senders, &mut summary, true)?;
        summary.pending_alerts = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
            .pending_count()?;
        Ok(summary)
    }

    /// Poll once, wait for queued Codex analysis to finish, then deliver alerts.
    ///
    /// Use this for one-shot command-line runs that exit immediately afterward.
    /// Continuous pollers should call [`Tracker::poll_once`] so Codex work does
    /// not delay the next NSE scan.
    ///
    /// # Errors
    /// Returns the same initialization, NSE warm-up, and database errors as
    /// [`Tracker::poll_once`]. Per-symbol NSE and delivery errors are included
    /// in the returned [`PollSummary`].
    pub fn poll_once_wait_for_codex(
        &self,
        senders: &[&dyn AlertSink],
        send_existing: bool,
    ) -> Result<PollSummary> {
        if self.codex_queue.is_none() {
            return self.poll_once(senders, send_existing);
        }

        let mut summary = self.poll_once(&[], send_existing)?;
        if let Some(queue) = self.codex_queue.as_ref()
            && let Err(error) = queue.wait_until_idle()
        {
            summary
                .errors
                .push(format!("Codex analysis wait: {error:#}"));
        }
        self.deliver_pending(senders, &mut summary, false)?;
        summary.pending_alerts = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
            .pending_count()?;
        Ok(summary)
    }

    fn record_poll_result(
        &self,
        stock: &StockEntry,
        announcements: &[Announcement],
        send_existing: bool,
        summary: &mut PollSummary,
    ) -> Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?;
        let first_poll = !store.symbol_initialized(&stock.symbol)?;
        let baseline = first_poll && !send_existing;
        let added = store.record_announcements(&stock.symbol, announcements, baseline)?;
        if baseline {
            summary.baselined_announcements += added;
        } else {
            summary.new_announcements += added;
        }
        Ok(())
    }

    fn deliver_pending(
        &self,
        senders: &[&dyn AlertSink],
        summary: &mut PollSummary,
        extract_missing_pdfs: bool,
    ) -> Result<()> {
        let pending = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
            .pending_announcements()?;

        for mut item in pending {
            if !extract_missing_pdfs && item.attachment_url.is_some() && item.pdf_text.is_none() {
                continue;
            }
            if item.attachment_url.is_some() && item.pdf_text.is_none() {
                let attempt = self
                    .store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
                    .note_pdf_attempt(&item.symbol, &item.seq_id)?;
                let url = item.attachment_url.as_deref().unwrap_or_default();
                match self.nse.extract_attachment_text(url) {
                    Ok(text) => {
                        let text = clip_chars(&text, self.config.pdf_text_limit_chars);
                        self.store
                            .lock()
                            .map_err(|_| {
                                anyhow::anyhow!("announcement database lock was poisoned")
                            })?
                            .save_pdf_text(&item.symbol, &item.seq_id, &text, None)?;
                        item.pdf_text = Some(text);
                        item.pdf_error = None;
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        if attempt >= MAX_PDF_ATTEMPTS {
                            self.store
                                .lock()
                                .map_err(|_| {
                                    anyhow::anyhow!("announcement database lock was poisoned")
                                })?
                                .save_pdf_text(&item.symbol, &item.seq_id, "", Some(&message))?;
                            item.pdf_text = Some(String::new());
                            item.pdf_error = Some(message);
                        } else {
                            self.store
                                .lock()
                                .map_err(|_| {
                                    anyhow::anyhow!("announcement database lock was poisoned")
                                })?
                                .save_pdf_error(&item.symbol, &item.seq_id, &message)?;
                            summary.errors.push(format!(
                                "{} announcement {} PDF attempt {attempt}/{MAX_PDF_ATTEMPTS}: {message}",
                                item.symbol, item.seq_id
                            ));
                            continue;
                        }
                    }
                }
            } else if item.attachment_url.is_none() && item.pdf_text.is_none() {
                self.store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("announcement database lock was poisoned"))?
                    .save_pdf_text(&item.symbol, &item.seq_id, "", None)?;
                item.pdf_text = Some(String::new());
            }

            if !item.codex_attempted
                && let Some(queue) = self.codex_queue.as_ref()
                && let Some(text) = item
                    .pdf_text
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
            {
                match queue.enqueue(&item.symbol, &item.seq_id, text) {
                    Ok(()) => continue,
                    Err(error) => {
                        let message = format!("{error:#}");
                        self.store
                            .lock()
                            .map_err(|_| {
                                anyhow::anyhow!("announcement database lock was poisoned")
                            })?
                            .save_codex_analysis(
                                &item.symbol,
                                &item.seq_id,
                                None,
                                Some(&message),
                            )?;
                        item.codex_error = Some(message.clone());
                        item.codex_attempted = true;
                        summary.errors.push(format!(
                            "{} announcement {} Codex analysis: {message}",
                            item.symbol, item.seq_id
                        ));
                    }
                }
            }

            let message = alert_message(&item);
            let require_whatsapp = senders
                .iter()
                .any(|sender| sender.channel() == DeliveryChannel::WhatsApp);
            let require_telegram = senders
                .iter()
                .any(|sender| sender.channel() == DeliveryChannel::Telegram);
            let mut attempted_whatsapp = false;
            let mut attempted_telegram = false;

            for sender in senders {
                let channel = sender.channel();
                let (already_sent, already_attempted) = match channel {
                    DeliveryChannel::WhatsApp => (&mut item.whatsapp_sent, &mut attempted_whatsapp),
                    DeliveryChannel::Telegram => (&mut item.telegram_sent, &mut attempted_telegram),
                };
                if *already_sent || *already_attempted {
                    continue;
                }
                *already_attempted = true;

                match sender.send_text(&message) {
                    Ok(()) => {
                        self.store
                            .lock()
                            .map_err(|_| {
                                anyhow::anyhow!("announcement database lock was poisoned")
                            })?
                            .mark_channel_sent(
                                &item.symbol,
                                &item.seq_id,
                                channel,
                                require_whatsapp,
                                require_telegram,
                            )?;
                        *already_sent = true;
                        summary.messages_sent += 1;
                    }
                    Err(error) => summary.errors.push(format!(
                        "{} {} delivery: {error:#}",
                        item.symbol,
                        channel_name(channel)
                    )),
                }
            }
        }
        Ok(())
    }
}

/// Counts and diagnostics produced by one polling cycle.
#[derive(Debug, Clone, Default)]
pub struct PollSummary {
    /// Number of symbols loaded from the current watchlist.
    pub symbols_checked: usize,
    /// Newly discovered filings queued for alert delivery.
    pub new_announcements: usize,
    /// Existing filings recorded silently during a symbol's first poll.
    pub baselined_announcements: usize,
    /// Alert messages accepted by configured delivery channels during this cycle.
    pub messages_sent: usize,
    /// Pending alerts retained locally for a later delivery attempt.
    pub pending_alerts: usize,
    /// NSE, PDF, or delivery errors that did not stop the entire polling cycle.
    pub errors: Vec<String>,
}

fn alert_message(item: &PendingAnnouncement) -> String {
    let mut message = String::from("NSE CORPORATE ANNOUNCEMENT\n");
    message.push_str(&item.symbol);
    if let Some(name) = item
        .company_name
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        message.push_str(" — ");
        message.push_str(&clip_chars(name, 120));
    }
    message.push_str("\nType: ");
    message.push_str(&clip_chars(&item.description, 240));
    message.push_str("\nDate: ");
    message.push_str(&clip_chars(&item.date, 80));

    if let Some(summary) = item.summary.as_deref().filter(|value| !value.is_empty()) {
        message.push_str("\nNSE summary: ");
        message.push_str(&clip_chars(summary, 500));
    }
    if let Some(error) = item.pdf_error.as_deref() {
        message.push_str("\nPDF text could not be extracted after three attempts: ");
        message.push_str(&clip_chars(error, 300));
    }
    if let Some(url) = item.attachment_url.as_deref() {
        message.push_str("\nPDF: ");
        if url.chars().count() <= 1_000 {
            message.push_str(url);
        } else {
            message.push_str("link omitted because the NSE URL is unusually long");
        }
    }

    if let Some(analysis) = item
        .codex_summary
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        message.push_str("\nCodex PDF analysis:\n");
        message.push_str(&clip_chars(analysis, 1_800));
    } else if let Some(text) = item.pdf_text.as_deref().filter(|value| !value.is_empty()) {
        if let Some(error) = item.codex_error.as_deref() {
            message.push_str("\nCodex analysis unavailable; showing extracted PDF text instead: ");
            message.push_str(&clip_chars(error, 250));
        }
        let label = "\nPDF text: ";
        let used = message.chars().count() + label.chars().count();
        let allowance = MAX_ALERT_MESSAGE_CHARS.saturating_sub(used).min(2_800);
        if allowance > 0 {
            message.push_str(label);
            message.push_str(&clip_chars(text, allowance));
        }
    }
    message
}

fn channel_name(channel: DeliveryChannel) -> &'static str {
    match channel {
        DeliveryChannel::WhatsApp => "WhatsApp",
        DeliveryChannel::Telegram => "Telegram",
    }
}

/// Normalize and validate a symbol using the same rules as [`Tracker::add_stock`].
pub fn validate_symbol(symbol: &str) -> Result<String> {
    normalize_symbol(symbol)
}

/// Parse a watchlist file without creating or modifying it.
pub fn read_watchlist(path: impl AsRef<Path>) -> Result<Vec<StockEntry>> {
    let path = path.as_ref();
    let list = StockList::load_or_default(path)?;
    if !path.exists() {
        bail!("watchlist does not exist: {}", path.display());
    }
    Ok(list.entries)
}
