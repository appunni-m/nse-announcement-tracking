mod keychain;

use eframe::egui::{self, Align, Color32, Layout, RichText, ScrollArea, TextEdit};
use nse_tracker::{
    AlertSink, AnnouncementRecord, MarketApiType, StockEntry, TelegramClient, TelegramSender,
    Tracker, TrackerConfig, WhatsAppEvent, WhatsAppSender,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

enum ControlCommand {
    Add(String, MarketApiType),
    Remove(String),
    PollNow,
    ConfigureWhatsApp {
        pair_phone: String,
        alert_phone: String,
    },
    DisableWhatsApp,
    FindTelegramChats {
        bot_token: Option<String>,
    },
    TelegramChatsFound {
        token: String,
        bot_username: Option<String>,
        chat_ids: Vec<i64>,
        error: Option<String>,
    },
    EnableTelegramChat(i64),
    DisableTelegram,
    TestWhatsApp,
    TestTelegram,
}

#[derive(Clone, Default)]
struct ViewState {
    status: String,
    last_poll: String,
    stocks: Vec<StockEntry>,
    announcements: Vec<AnnouncementRecord>,
    pairing_code: Option<String>,
    whatsapp_configured: bool,
    whatsapp_connected: bool,
    whatsapp_status: String,
    telegram_token_configured: bool,
    telegram_chat_id: Option<i64>,
    telegram_bot_username: Option<String>,
    telegram_chat_ids: Vec<i64>,
    telegram_status: String,
    messaging_notice: Option<String>,
    warning: Option<String>,
}

struct WidgetApp {
    state: Arc<Mutex<ViewState>>,
    commands: Sender<ControlCommand>,
    whatsapp_pair_phone: String,
    whatsapp_alert_phone: String,
    telegram_bot_token: String,
    selected_telegram_chat_id: Option<i64>,
    show_messaging_setup: bool,
    symbol_input: String,
    market: MarketApiType,
}

impl WidgetApp {
    fn start(
        config: TrackerConfig,
        keychain_migrated: bool,
        keychain_warning: Option<String>,
    ) -> Self {
        let whatsapp_pair_phone = config.pair_phone.clone().unwrap_or_default();
        let whatsapp_alert_phone = match (&config.pair_phone, &config.alert_phone) {
            (Some(pair), Some(alert)) if pair != alert => alert.clone(),
            _ => String::new(),
        };
        let telegram_token_configured = config.telegram_bot_token.is_some();
        let telegram_chat_id = config
            .telegram_chat_id
            .as_deref()
            .and_then(|id| id.parse::<i64>().ok());
        let state = Arc::new(Mutex::new(ViewState {
            status: "Starting tracker…".to_owned(),
            whatsapp_configured: config.pair_phone.is_some(),
            whatsapp_status: if config.pair_phone.is_some() {
                "Connecting…".to_owned()
            } else {
                "Not set up".to_owned()
            },
            telegram_token_configured,
            telegram_chat_id,
            telegram_status: channel_status(telegram_token_configured, telegram_chat_id.is_some()),
            messaging_notice: keychain_migrated
                .then(|| "Existing messaging settings were moved into macOS Keychain.".to_owned()),
            warning: keychain_warning,
            ..ViewState::default()
        }));
        let (commands, command_rx) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker_commands = commands.clone();

        thread::Builder::new()
            .name("nse-tracker-worker".to_owned())
            .spawn(move || {
                run_worker(config, worker_state, command_rx, worker_commands);
            })
            .expect("could not start the tracker worker thread");

        Self {
            state,
            commands,
            whatsapp_pair_phone,
            whatsapp_alert_phone,
            telegram_bot_token: String::new(),
            selected_telegram_chat_id: telegram_chat_id,
            show_messaging_setup: false,
            symbol_input: String::new(),
            market: MarketApiType::Equities,
        }
    }

    fn send_command(&self, command: ControlCommand) {
        if self.commands.send(command).is_err() {
            if let Ok(mut state) = self.state.lock() {
                state.warning = Some("The tracker worker has stopped.".to_owned());
            }
        }
    }
}

impl eframe::App for WidgetApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root_ui.ctx().clone();
        context.request_repaint_after(Duration::from_millis(750));
        let state = match self.state.lock() {
            Ok(state) => state.clone(),
            Err(_) => return,
        };

        egui::CentralPanel::default().show(root_ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("NSE Announcements");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Poll now").clicked() {
                        self.send_command(ControlCommand::PollNow);
                    }
                    if ui.button("Set up alerts").clicked() {
                        self.show_messaging_setup = true;
                    }
                });
            });
            ui.label(RichText::new(&state.status).color(Color32::LIGHT_BLUE));
            ui.small(format!(
                "WhatsApp: {} · Telegram: {}",
                state.whatsapp_status, state.telegram_status
            ));
            if !state.last_poll.is_empty() {
                ui.small(format!("Last poll: {}", state.last_poll));
            }
            if let Some(code) = state.pairing_code.as_deref() {
                ui.separator();
                ui.label(
                    RichText::new(format!("WhatsApp pairing code: {code}"))
                        .strong()
                        .color(Color32::YELLOW),
                );
                ui.small("On your phone: WhatsApp → Linked devices → Link with phone number.");
            }
            if let Some(warning) = state.warning.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, warning);
            }

            ui.separator();
            ui.heading("Tracked symbols");
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.symbol_input).hint_text("NSE symbol"));
                egui::ComboBox::from_id_salt("market-type")
                    .selected_text(self.market.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.market, MarketApiType::Equities, "equities");
                        ui.selectable_value(&mut self.market, MarketApiType::Sme, "sme");
                    });
                if ui.button("Add").clicked() {
                    let symbol = self.symbol_input.trim().to_owned();
                    if !symbol.is_empty() {
                        self.send_command(ControlCommand::Add(symbol, self.market));
                        self.symbol_input.clear();
                    }
                }
            });
            if state.stocks.is_empty() {
                ui.label("No symbols are configured.");
            } else {
                for stock in &state.stocks {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&stock.symbol).strong());
                        ui.small(stock.market_api_type.as_str());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                self.send_command(ControlCommand::Remove(stock.symbol.clone()));
                            }
                        });
                    });
                }
            }

            ui.separator();
            ui.heading("Recent announcements");
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if state.announcements.is_empty() {
                        ui.label("No announcements have been recorded yet.");
                    }
                    for item in &state.announcements {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&item.symbol).strong());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.small(&item.date);
                                });
                            });
                            ui.label(&item.description);
                            if let Some(company) = item.company_name.as_deref() {
                                ui.small(company);
                            }
                            if let Some(analysis) = item
                                .codex_summary
                                .as_deref()
                                .filter(|text| !text.trim().is_empty())
                            {
                                ui.label(RichText::new("Codex PDF analysis").strong());
                                ui.small(analysis);
                                if let Some(text) = item
                                    .pdf_text
                                    .as_deref()
                                    .filter(|text| !text.trim().is_empty())
                                {
                                    ui.collapsing("Extracted PDF text", |ui| {
                                        ui.small(text);
                                    });
                                }
                            } else if let Some(text) = item
                                .pdf_text
                                .as_deref()
                                .filter(|text| !text.trim().is_empty())
                            {
                                ui.small(text);
                                if let Some(error) = item.codex_error.as_deref() {
                                    ui.colored_label(
                                        Color32::LIGHT_RED,
                                        format!("Codex analysis unavailable: {error}"),
                                    );
                                }
                            } else if let Some(summary) = item.summary.as_deref() {
                                ui.small(summary);
                            }
                            ui.small(format!("Alert status: {}", item.status));
                        });
                        ui.add_space(5.0);
                    }
                });
        });

        if self.show_messaging_setup {
            let mut show_messaging_setup = self.show_messaging_setup;
            egui::Window::new("Set up message alerts")
                .open(&mut show_messaging_setup)
                .default_width(520.0)
                .resizable(true)
                .show(&context, |ui| {
                    ui.label("Connect a destination for new NSE announcement alerts.");

                    ui.separator();
                    ui.heading("WhatsApp");
                    ui.label("Enter the WhatsApp number that will link this device. Include the country code.");
                    ui.add(
                        TextEdit::singleline(&mut self.whatsapp_pair_phone)
                            .hint_text("For example: 919876543210"),
                    );
                    ui.label("Send alerts to");
                    ui.add(
                        TextEdit::singleline(&mut self.whatsapp_alert_phone)
                            .hint_text("Leave blank to message the linked number"),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.whatsapp_pair_phone.trim().is_empty()
                                    && state.whatsapp_status != "Connecting…",
                                egui::Button::new("Save & connect WhatsApp"),
                            )
                            .clicked()
                        {
                            self.send_command(ControlCommand::ConfigureWhatsApp {
                                pair_phone: self.whatsapp_pair_phone.clone(),
                                alert_phone: self.whatsapp_alert_phone.clone(),
                            });
                        }
                        if ui
                            .add_enabled(
                                state.whatsapp_configured,
                                egui::Button::new("Disable alerts"),
                            )
                            .clicked()
                        {
                            self.send_command(ControlCommand::DisableWhatsApp);
                        }
                        if ui
                            .add_enabled(state.whatsapp_connected, egui::Button::new("Send test"))
                            .clicked()
                        {
                            self.send_command(ControlCommand::TestWhatsApp);
                        }
                    });
                    ui.small(format!("Status: {}", state.whatsapp_status));
                    if let Some(code) = state.pairing_code.as_deref() {
                        ui.label(
                            RichText::new(format!("Pairing code: {code}"))
                                .strong()
                                .color(Color32::YELLOW),
                        );
                        ui.small("On your phone, open WhatsApp → Linked devices → Link with phone number.");
                    }
                    ui.small("WhatsApp linking uses an unofficial linked-device client. The phone numbers are saved in macOS Keychain; its linked-device session remains in the private local tracker folder.");
                    ui.small("Disabling alerts keeps the linked-device session on this Mac so you can reconnect without pairing again.");

                    ui.separator();
                    ui.heading("Telegram");
                    ui.label("Create a bot with @BotFather. Open that bot in Telegram and send it /start before searching for chats.");
                    ui.add(
                        TextEdit::singleline(&mut self.telegram_bot_token)
                            .password(true)
                            .hint_text(if state.telegram_token_configured {
                                "Paste a new token, or leave blank to use the saved token"
                            } else {
                                "Paste the bot token from @BotFather"
                            }),
                    );
                    if ui
                        .add_enabled(
                            state.telegram_status != "Searching for private chats…",
                            egui::Button::new("Find my Telegram chats"),
                        )
                        .clicked()
                    {
                        let token = (!self.telegram_bot_token.trim().is_empty())
                            .then(|| self.telegram_bot_token.trim().to_owned());
                        if token.is_some() || state.telegram_token_configured {
                            self.send_command(ControlCommand::FindTelegramChats {
                                bot_token: token,
                            });
                            self.telegram_bot_token.clear();
                        } else if let Ok(mut view) = self.state.lock() {
                            view.messaging_notice = Some(
                                "Paste your BotFather token first, then search for chats.".to_owned(),
                            );
                        }
                    }
                    if let Some(username) = state.telegram_bot_username.as_deref() {
                        ui.small(format!("Verified bot: @{username}"));
                    }
                    if !state.telegram_chat_ids.is_empty() {
                        ui.label("Choose the private chat that should receive alerts:");
                        for chat_id in &state.telegram_chat_ids {
                            ui.radio_value(
                                &mut self.selected_telegram_chat_id,
                                Some(*chat_id),
                                format!("Private chat · {chat_id}"),
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                state.telegram_token_configured
                                    && self
                                        .selected_telegram_chat_id
                                        .is_some_and(|id| state.telegram_chat_ids.contains(&id)),
                                egui::Button::new("Enable selected chat"),
                            )
                            .clicked()
                        {
                            if let Some(chat_id) = self.selected_telegram_chat_id {
                                self.send_command(ControlCommand::EnableTelegramChat(chat_id));
                            }
                        }
                        if ui
                            .add_enabled(
                                state.telegram_token_configured || state.telegram_chat_id.is_some(),
                                egui::Button::new("Disable"),
                            )
                            .clicked()
                        {
                            self.send_command(ControlCommand::DisableTelegram);
                        }
                        if ui
                            .add_enabled(
                                state.telegram_token_configured
                                    && state.telegram_chat_id.is_some(),
                                egui::Button::new("Send test"),
                            )
                            .clicked()
                        {
                            self.send_command(ControlCommand::TestTelegram);
                        }
                    });
                    ui.small(format!("Status: {}", state.telegram_status));
                    if let Some(notice) = state.messaging_notice.as_deref() {
                        ui.colored_label(Color32::LIGHT_BLUE, notice);
                    }
                    ui.small("Channel secrets are stored in macOS Keychain. Test messages are sent only when you choose Send test.");
                });
            self.show_messaging_setup = show_messaging_setup;
        }
    }
}

fn channel_status(token_configured: bool, chat_configured: bool) -> String {
    match (token_configured, chat_configured) {
        (true, true) => "Ready".to_owned(),
        (true, false) => "Choose a destination chat".to_owned(),
        (false, true) => "Needs a bot token".to_owned(),
        (false, false) => "Not set up".to_owned(),
    }
}

fn run_worker(
    mut config: TrackerConfig,
    state: Arc<Mutex<ViewState>>,
    commands: Receiver<ControlCommand>,
    command_sender: Sender<ControlCommand>,
) {
    let tracker = match Tracker::new(config.clone()) {
        Ok(tracker) => tracker,
        Err(error) => {
            update_state(&state, |view| {
                view.status = "Tracker failed to start".to_owned();
                view.warning = Some(format!("{error:#}"));
            });
            return;
        }
    };

    let (mut whatsapp_sender, mut whatsapp_events) =
        connect_whatsapp_if_configured(&config, &state);
    let mut telegram_sender = connect_telegram_if_configured(&config, &state);
    let mut poll_now = true;
    let mut next_poll = Instant::now();

    loop {
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    handle_command(
                        command,
                        &tracker,
                        &mut config,
                        &state,
                        &mut whatsapp_sender,
                        &mut whatsapp_events,
                        &mut telegram_sender,
                        &command_sender,
                        &mut poll_now,
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        consume_whatsapp_events(&state, whatsapp_events.as_ref());

        if poll_now || Instant::now() >= next_poll {
            poll_now = false;
            update_state(&state, |view| view.status = "Polling NSE…".to_owned());
            let mut sinks: Vec<&dyn AlertSink> = Vec::new();
            if let Some(sender) = whatsapp_sender.as_ref() {
                sinks.push(sender);
            }
            if let Some(sender) = telegram_sender.as_ref() {
                sinks.push(sender);
            }
            match tracker.poll_once(&sinks, false) {
                Ok(summary) => {
                    let recent = tracker.recent_announcements(40).unwrap_or_default();
                    let stocks = tracker.stock_list().unwrap_or_default();
                    update_state(&state, |view| {
                        view.status = format!(
                            "{} symbols · {} pending alert{}",
                            summary.symbols_checked,
                            summary.pending_alerts,
                            if summary.pending_alerts == 1 { "" } else { "s" }
                        );
                        view.last_poll = chrono::Local::now().format("%H:%M:%S").to_string();
                        view.stocks = stocks;
                        view.announcements = recent;
                        view.warning = if summary.errors.is_empty() {
                            None
                        } else {
                            Some(summary.errors.join("\n"))
                        };
                    });
                }
                Err(error) => update_state(&state, |view| {
                    view.status = "NSE poll failed".to_owned();
                    view.warning = Some(format!("{error:#}"));
                }),
            }
            next_poll = Instant::now() + Duration::from_secs(config.poll_interval_seconds);
            continue;
        }

        let remaining = next_poll.saturating_duration_since(Instant::now());
        match commands.recv_timeout(remaining.min(Duration::from_millis(300))) {
            Ok(command) => handle_command(
                command,
                &tracker,
                &mut config,
                &state,
                &mut whatsapp_sender,
                &mut whatsapp_events,
                &mut telegram_sender,
                &command_sender,
                &mut poll_now,
            ),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        consume_whatsapp_events(&state, whatsapp_events.as_ref());
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    command: ControlCommand,
    tracker: &Tracker,
    config: &mut TrackerConfig,
    state: &Arc<Mutex<ViewState>>,
    whatsapp_sender: &mut Option<WhatsAppSender>,
    whatsapp_events: &mut Option<Receiver<WhatsAppEvent>>,
    telegram_sender: &mut Option<TelegramSender>,
    command_sender: &Sender<ControlCommand>,
    poll_now: &mut bool,
) {
    match command {
        ControlCommand::Add(symbol, market) => match tracker.add_stock(&symbol, market) {
            Ok(()) => {
                *poll_now = true;
                update_state(state, |view| {
                    view.warning = None;
                    view.status = format!("Tracking {}", symbol.to_ascii_uppercase());
                });
            }
            Err(error) => update_state(state, |view| {
                view.warning = Some(format!("Could not add symbol: {error:#}"));
            }),
        },
        ControlCommand::Remove(symbol) => match tracker.remove_stock(&symbol) {
            Ok(true) => {
                *poll_now = true;
                update_state(state, |view| {
                    view.warning = None;
                    view.status = format!("Removed {}", symbol.to_ascii_uppercase());
                });
            }
            Ok(false) => {}
            Err(error) => update_state(state, |view| {
                view.warning = Some(format!("Could not remove symbol: {error:#}"));
            }),
        },
        ControlCommand::PollNow => *poll_now = true,
        ControlCommand::ConfigureWhatsApp {
            pair_phone,
            alert_phone,
        } => configure_whatsapp(
            pair_phone,
            alert_phone,
            config,
            state,
            whatsapp_sender,
            whatsapp_events,
        ),
        ControlCommand::DisableWhatsApp => {
            let pair_result =
                keychain::save(keychain::WHATSAPP_PAIR_PHONE, keychain::disable_marker());
            let alert_result =
                keychain::save(keychain::WHATSAPP_ALERT_PHONE, keychain::disable_marker());
            let cleanup_result = if pair_result.is_ok() && alert_result.is_ok() {
                keychain::redact_legacy_file(&config.root_dir)
            } else {
                Ok(())
            };
            whatsapp_sender.take();
            whatsapp_events.take();
            config.pair_phone = None;
            config.alert_phone = None;
            update_state(state, |view| {
                view.whatsapp_configured = false;
                view.whatsapp_connected = false;
                view.whatsapp_status = "Disabled".to_owned();
                view.pairing_code = None;
                view.messaging_notice = Some("WhatsApp alerts disabled.".to_owned());
                view.warning =
                    first_error(pair_result, alert_result).or_else(|| cleanup_result.err());
            });
        }
        ControlCommand::FindTelegramChats { bot_token } => {
            let token = bot_token.or_else(|| config.telegram_bot_token.clone());
            let Some(token) = token else {
                update_state(state, |view| {
                    view.messaging_notice =
                        Some("Paste your BotFather token before searching for chats.".to_owned());
                });
                return;
            };
            let client = match TelegramClient::new(&token) {
                Ok(client) => client,
                Err(error) => {
                    update_state(state, |view| {
                        view.messaging_notice = Some(format!("Telegram setup failed: {error:#}"));
                    });
                    return;
                }
            };
            update_state(state, |view| {
                view.telegram_status = "Searching for private chats…".to_owned();
                view.telegram_chat_ids.clear();
                view.messaging_notice = Some(
                    "Searching for recent private chats. This can take up to 25 seconds."
                        .to_owned(),
                );
            });
            let sender = command_sender.clone();
            let worker_token = token.clone();
            if let Err(error) = thread::Builder::new()
                .name("telegram-chat-discovery".to_owned())
                .spawn(move || {
                    let result = client
                        .get_bot_username()
                        .and_then(|username| {
                            client
                                .discover_private_chat_ids(25)
                                .map(|chat_ids| (username, chat_ids))
                        })
                        .map_err(|error| format!("{error:#}"));
                    let (bot_username, chat_ids, error) = match result {
                        Ok((username, chat_ids)) => (username, chat_ids, None),
                        Err(error) => (None, Vec::new(), Some(error)),
                    };
                    let _ = sender.send(ControlCommand::TelegramChatsFound {
                        token: worker_token,
                        bot_username,
                        chat_ids,
                        error,
                    });
                })
            {
                update_state(state, |view| {
                    view.telegram_status = "Setup failed".to_owned();
                    view.messaging_notice =
                        Some(format!("Could not start Telegram chat search: {error}"));
                });
            }
        }
        ControlCommand::TelegramChatsFound {
            token,
            bot_username,
            chat_ids,
            error,
        } => {
            if let Some(error) = error {
                update_state(state, |view| {
                    view.telegram_status = "Setup failed".to_owned();
                    view.messaging_notice = Some(format!("Telegram setup failed: {error}"));
                });
                return;
            }
            let token_changed = config.telegram_bot_token.as_deref() != Some(token.as_str());
            if token_changed {
                if let Err(error) =
                    keychain::save(keychain::TELEGRAM_CHAT_ID, keychain::disable_marker())
                {
                    update_state(state, |view| {
                        view.telegram_status = "Setup failed".to_owned();
                        view.messaging_notice = Some(format!(
                            "Could not safely replace the saved Telegram destination: {error}"
                        ));
                    });
                    return;
                }
            }
            if let Err(error) = keychain::save(keychain::TELEGRAM_BOT_TOKEN, &token) {
                update_state(state, |view| {
                    view.telegram_status = "Setup failed".to_owned();
                    view.messaging_notice = Some(format!("Could not save the bot token: {error}"));
                });
                return;
            }
            config.telegram_bot_token = Some(token);
            if token_changed {
                config.telegram_chat_id = None;
                telegram_sender.take();
            }
            let redaction_error = keychain::redact_legacy_file(&config.root_dir).err();
            let chat_count = chat_ids.len();
            let has_current_destination = config.telegram_chat_id.is_some();
            update_state(state, |view| {
                view.telegram_token_configured = true;
                view.telegram_bot_username = bot_username;
                view.telegram_chat_ids = chat_ids;
                view.telegram_chat_id = config
                    .telegram_chat_id
                    .as_deref()
                    .and_then(|id| id.parse::<i64>().ok());
                view.telegram_status = channel_status(true, has_current_destination);
                view.messaging_notice = Some(if chat_count == 0 {
                    "Bot verified. No private chat was found yet; open the bot in Telegram, send /start, then search again.".to_owned()
                } else if has_current_destination && !token_changed {
                    format!(
                        "Bot verified. Found {chat_count} private chat(s); your saved destination is still enabled."
                    )
                } else {
                    format!(
                        "Bot verified. Found {chat_count} private chat(s); select one and enable it."
                    )
                });
                view.warning = redaction_error.map(|error| {
                    format!(
                        "The token is saved in Keychain, but legacy .env cleanup failed: {error}"
                    )
                });
            });
        }
        ControlCommand::EnableTelegramChat(chat_id) => {
            let is_discovered = state
                .lock()
                .map(|view| view.telegram_chat_ids.contains(&chat_id))
                .unwrap_or(false);
            if !is_discovered || config.telegram_bot_token.is_none() {
                update_state(state, |view| {
                    view.messaging_notice = Some(
                        "Search and choose a private chat before enabling Telegram.".to_owned(),
                    );
                });
                return;
            }
            if let Err(error) = keychain::save(keychain::TELEGRAM_CHAT_ID, &chat_id.to_string()) {
                update_state(state, |view| {
                    view.messaging_notice = Some(format!("Could not save the chat: {error}"));
                });
                return;
            }
            config.telegram_chat_id = Some(chat_id.to_string());
            telegram_sender.take();
            let redaction_error = keychain::redact_legacy_file(&config.root_dir).err();
            match TelegramSender::connect(config) {
                Ok(sender) => {
                    *telegram_sender = Some(sender);
                    update_state(state, |view| {
                        view.telegram_token_configured = true;
                        view.telegram_chat_id = Some(chat_id);
                        view.telegram_status = "Ready".to_owned();
                        view.messaging_notice = Some(format!(
                            "Telegram alerts enabled for private chat {chat_id}."
                        ));
                        view.warning = redaction_error.map(|error| {
                            format!("Credentials are saved in Keychain, but legacy .env cleanup failed: {error}")
                        });
                    });
                }
                Err(error) => update_state(state, |view| {
                    view.telegram_status = "Could not connect".to_owned();
                    view.messaging_notice = Some(format!("Telegram setup failed: {error:#}"));
                }),
            }
        }
        ControlCommand::DisableTelegram => {
            let token_result =
                keychain::save(keychain::TELEGRAM_BOT_TOKEN, keychain::disable_marker());
            let chat_result =
                keychain::save(keychain::TELEGRAM_CHAT_ID, keychain::disable_marker());
            let cleanup_result = if token_result.is_ok() && chat_result.is_ok() {
                keychain::redact_legacy_file(&config.root_dir)
            } else {
                Ok(())
            };
            telegram_sender.take();
            config.telegram_bot_token = None;
            config.telegram_chat_id = None;
            update_state(state, |view| {
                view.telegram_token_configured = false;
                view.telegram_chat_id = None;
                view.telegram_bot_username = None;
                view.telegram_chat_ids.clear();
                view.telegram_status = "Disabled".to_owned();
                view.messaging_notice = Some("Telegram alerts disabled.".to_owned());
                view.warning =
                    first_error(token_result, chat_result).or_else(|| cleanup_result.err());
            });
        }
        ControlCommand::TestWhatsApp => match whatsapp_sender.as_ref() {
            Some(sender) => match sender
                .send_text("NSE Tracker test message. WhatsApp alert delivery is set up.")
            {
                Ok(()) => update_state(state, |view| {
                    view.messaging_notice = Some("WhatsApp test message sent.".to_owned());
                    view.warning = None;
                }),
                Err(error) => update_state(state, |view| {
                    view.messaging_notice = Some(format!("WhatsApp test failed: {error:#}"));
                }),
            },
            None => update_state(state, |view| {
                view.messaging_notice = Some("Connect WhatsApp before sending a test.".to_owned());
            }),
        },
        ControlCommand::TestTelegram => match telegram_sender.as_ref() {
            Some(sender) => match sender
                .send_text("NSE Tracker test message. Telegram alert delivery is set up.")
            {
                Ok(()) => update_state(state, |view| {
                    view.messaging_notice = Some("Telegram test message sent.".to_owned());
                    view.warning = None;
                }),
                Err(error) => update_state(state, |view| {
                    view.messaging_notice = Some(format!("Telegram test failed: {error:#}"));
                }),
            },
            None => update_state(state, |view| {
                view.messaging_notice =
                    Some("Enable a Telegram chat before sending a test.".to_owned());
            }),
        },
    }
}

fn configure_whatsapp(
    pair_phone: String,
    alert_phone: String,
    config: &mut TrackerConfig,
    state: &Arc<Mutex<ViewState>>,
    whatsapp_sender: &mut Option<WhatsAppSender>,
    whatsapp_events: &mut Option<Receiver<WhatsAppEvent>>,
) {
    let pair_phone = match normalize_phone(&pair_phone) {
        Ok(phone) => phone,
        Err(error) => {
            update_state(state, |view| view.messaging_notice = Some(error));
            return;
        }
    };
    let alert_phone = if alert_phone.trim().is_empty() {
        pair_phone.clone()
    } else {
        match normalize_phone(&alert_phone) {
            Ok(phone) => phone,
            Err(error) => {
                update_state(state, |view| view.messaging_notice = Some(error));
                return;
            }
        }
    };
    if let Err(error) = keychain::save(keychain::WHATSAPP_PAIR_PHONE, &pair_phone) {
        update_state(state, |view| {
            view.messaging_notice = Some(format!("Could not save the WhatsApp number: {error}"));
        });
        return;
    }
    if let Err(error) = keychain::save(keychain::WHATSAPP_ALERT_PHONE, &alert_phone) {
        update_state(state, |view| {
            view.messaging_notice = Some(format!("Could not save the alert number: {error}"));
        });
        return;
    }
    config.pair_phone = Some(pair_phone);
    config.alert_phone = Some(alert_phone);
    whatsapp_sender.take();
    whatsapp_events.take();
    let redaction_error = keychain::redact_legacy_file(&config.root_dir).err();
    update_state(state, |view| {
        view.whatsapp_configured = true;
        view.whatsapp_connected = false;
        view.whatsapp_status = "Connecting…".to_owned();
        view.pairing_code = None;
        view.messaging_notice = Some("WhatsApp phone numbers saved in macOS Keychain.".to_owned());
        view.warning = redaction_error.map(|error| {
            format!("WhatsApp is configured in Keychain, but legacy .env cleanup failed: {error}")
        });
    });
    match connect_whatsapp(config) {
        Ok((sender, events)) => {
            *whatsapp_sender = Some(sender);
            *whatsapp_events = Some(events);
        }
        Err(error) => update_state(state, |view| {
            view.whatsapp_status = "Could not connect".to_owned();
            view.warning = Some(format!("WhatsApp connection failed: {error:#}"));
        }),
    }
}

fn connect_whatsapp_if_configured(
    config: &TrackerConfig,
    state: &Arc<Mutex<ViewState>>,
) -> (Option<WhatsAppSender>, Option<Receiver<WhatsAppEvent>>) {
    if config.pair_phone.is_none() {
        return (None, None);
    }
    match connect_whatsapp(config) {
        Ok((sender, events)) => (Some(sender), Some(events)),
        Err(error) => {
            update_state(state, |view| {
                view.whatsapp_status = "Could not connect".to_owned();
                view.warning = Some(format!("WhatsApp connection failed: {error:#}"));
            });
            (None, None)
        }
    }
}

fn connect_whatsapp(
    config: &TrackerConfig,
) -> Result<(WhatsAppSender, Receiver<WhatsAppEvent>), String> {
    let (event_sender, event_receiver) = mpsc::channel();
    let sender =
        WhatsAppSender::connect(config, event_sender).map_err(|error| format!("{error:#}"))?;
    Ok((sender, event_receiver))
}

fn connect_telegram_if_configured(
    config: &TrackerConfig,
    state: &Arc<Mutex<ViewState>>,
) -> Option<TelegramSender> {
    if config.telegram_bot_token.is_none() || config.telegram_chat_id.is_none() {
        return None;
    }
    match TelegramSender::connect(config) {
        Ok(sender) => Some(sender),
        Err(error) => {
            update_state(state, |view| {
                view.telegram_status = "Could not connect".to_owned();
                view.warning = Some(format!("Telegram setup failed: {error:#}"));
            });
            None
        }
    }
}

fn consume_whatsapp_events(
    state: &Arc<Mutex<ViewState>>,
    events: Option<&Receiver<WhatsAppEvent>>,
) {
    let Some(events) = events else { return };
    loop {
        match events.try_recv() {
            Ok(WhatsAppEvent::PairingCode {
                code,
                timeout_seconds,
            }) => update_state(state, |view| {
                view.pairing_code = Some(code);
                view.whatsapp_status = "Needs phone pairing".to_owned();
                view.messaging_notice = Some(format!(
                    "Pair this device in WhatsApp within {timeout_seconds} seconds."
                ));
            }),
            Ok(WhatsAppEvent::Connected) => update_state(state, |view| {
                view.pairing_code = None;
                view.whatsapp_configured = true;
                view.whatsapp_connected = true;
                view.whatsapp_status = "Connected".to_owned();
                view.messaging_notice = Some("WhatsApp is ready for alerts.".to_owned());
            }),
            Ok(WhatsAppEvent::LoggedOut) => update_state(state, |view| {
                view.whatsapp_connected = false;
                view.whatsapp_status = "Logged out · reconnect required".to_owned();
                view.messaging_notice = Some(
                    "WhatsApp logged this linked device out. Save & connect again to pair it."
                        .to_owned(),
                );
            }),
            Ok(WhatsAppEvent::PairingFailed { message }) => update_state(state, |view| {
                view.whatsapp_connected = false;
                view.whatsapp_status = "Pairing failed".to_owned();
                view.messaging_notice = Some(format!("WhatsApp pairing failed: {message}"));
            }),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}

fn normalize_phone(raw: &str) -> Result<String, String> {
    let phone: String = raw
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if !(8..=15).contains(&phone.len()) {
        return Err("Enter a phone number with country code and 8–15 digits.".to_owned());
    }
    Ok(phone)
}

fn first_error(first: Result<(), String>, second: Result<(), String>) -> Option<String> {
    first.err().or_else(|| second.err())
}

fn update_state(state: &Arc<Mutex<ViewState>>, update: impl FnOnce(&mut ViewState)) {
    if let Ok(mut state) = state.lock() {
        update(&mut state);
    }
}

fn main() -> eframe::Result<()> {
    let config_dir = config_dir();
    let mut config = TrackerConfig::from_dir(&config_dir).unwrap_or_else(|error| {
        eprintln!("Could not load settings: {error:#}");
        std::process::exit(2);
    });
    let (keychain_migrated, keychain_warning) = match keychain::load_into_config(&mut config) {
        Ok(migrated) => (migrated, None),
        Err(error) => {
            config.pair_phone = None;
            config.alert_phone = None;
            config.telegram_bot_token = None;
            config.telegram_chat_id = None;
            (
                false,
                Some(format!(
                    "Could not read messaging settings from macOS Keychain: {error}. Re-enter them in Set up alerts."
                )),
            )
        }
    };
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("NSE Announcements")
            .with_inner_size([480.0, 780.0])
            .with_min_inner_size([380.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "NSE Announcements",
        native_options,
        Box::new(move |_creation_context| {
            Ok(Box::new(WidgetApp::start(
                config,
                keychain_migrated,
                keychain_warning,
            )))
        }),
    )
}

fn config_dir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|argument| argument == "--config-dir") {
        if let Some(path) = args.get(index + 1) {
            return PathBuf::from(path);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
