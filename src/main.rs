use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use nse_tracker::{
    AlertSink, MarketApiType, TelegramClient, TelegramSender, Tracker, TrackerConfig,
    WhatsAppEvent, WhatsAppSender,
};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(
    name = "nse-tracker",
    version,
    about = "Track NSE corporate announcements and send WhatsApp or Telegram alerts"
)]
struct Cli {
    /// Directory containing .env, stocks.json, and .nse-tracker state.
    #[arg(long, global = true, default_value = ".")]
    config_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the default watchlist and local state directory.
    Init,
    /// Add, remove, or list NSE symbols.
    Stock {
        #[command(subcommand)]
        command: StockCommand,
    },
    /// Poll once. On a symbol's first poll, historical filings are stored silently.
    PollOnce {
        /// Send filings in the initial lookback window instead of baselining them.
        #[arg(long)]
        send_existing: bool,
    },
    /// Poll continuously at the configured interval (60 seconds by default).
    Run {
        /// Send filings in the initial lookback window instead of baselining them.
        #[arg(long)]
        send_existing: bool,
    },
    /// Link this installation as a WhatsApp device using the phone-number code.
    WhatsappLink,
    /// Verify a Telegram bot and discover private chat IDs for alert delivery.
    TelegramLink,
    /// Install or remove the macOS LaunchAgent that starts polling at login.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

#[derive(Debug, Subcommand)]
enum StockCommand {
    /// Add a symbol. Main-board symbols use equities; SME symbols use sme.
    Add {
        symbol: String,
        #[arg(long, value_enum, default_value_t = MarketChoice::Equities)]
        market: MarketChoice,
    },
    /// Stop tracking a symbol.
    Remove { symbol: String },
    /// Show the current watchlist.
    List,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MarketChoice {
    Equities,
    Sme,
}

impl From<MarketChoice> for MarketApiType {
    fn from(value: MarketChoice) -> Self {
        match value {
            MarketChoice::Equities => Self::Equities,
            MarketChoice::Sme => Self::Sme,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Install and start a per-user macOS LaunchAgent.
    Install,
    /// Stop and remove the per-user macOS LaunchAgent.
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_dir = prepare_config_dir(&cli.config_dir)?;
    let config = TrackerConfig::from_dir(&config_dir)?;

    match cli.command {
        Command::Init => init(&config),
        Command::Stock { command } => manage_stocks(&config, command),
        Command::PollOnce { send_existing } => {
            let tracker = Tracker::new(config.clone())?;
            let (sender, events) = connect_optional(&config)?;
            let telegram = connect_telegram(&config)?;
            let sinks = configured_sinks(sender.as_ref(), telegram.as_ref());
            let result = tracker.poll_once_wait_for_codex(&sinks, send_existing)?;
            print_summary(&result);
            print_queued_events(&events);
            Ok(())
        }
        Command::Run { send_existing } => run(config, send_existing),
        Command::WhatsappLink => link_whatsapp(&config),
        Command::TelegramLink => link_telegram(&config),
        Command::Service { command } => manage_service(&config, command),
    }
}

fn prepare_config_dir(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path).with_context(|| {
        format!(
            "could not create configuration directory {}",
            path.display()
        )
    })?;
    path.canonicalize().with_context(|| {
        format!(
            "could not resolve configuration directory {}",
            path.display()
        )
    })
}

fn init(config: &TrackerConfig) -> Result<()> {
    let _tracker = Tracker::new(config.clone())?;
    println!("Initialized {}", config.root_dir.display());
    println!("Watchlist: {}", config.stocks_file.display());
    println!("Local state: {}", config.state_dir.display());
    if !config.root_dir.join(".env").exists() {
        println!("Copy .env.example to .env and configure WhatsApp and/or Telegram delivery.");
    }
    Ok(())
}

fn manage_stocks(config: &TrackerConfig, command: StockCommand) -> Result<()> {
    let tracker = Tracker::new(config.clone())?;
    match command {
        StockCommand::List => {
            let stocks = tracker.stock_list()?;
            if stocks.is_empty() {
                println!("No symbols are tracked.");
            } else {
                for stock in stocks {
                    println!("{}\t{}", stock.symbol, stock.market_api_type.as_str());
                }
            }
        }
        StockCommand::Add { symbol, market } => {
            tracker.add_stock(&symbol, market.into())?;
            println!("Added or updated {}.", symbol.trim().to_ascii_uppercase());
        }
        StockCommand::Remove { symbol } => {
            if tracker.remove_stock(&symbol)? {
                println!("Removed {}.", symbol.trim().to_ascii_uppercase());
            } else {
                println!(
                    "{} was not in the watchlist.",
                    symbol.trim().to_ascii_uppercase()
                );
            }
        }
    }
    Ok(())
}

fn connect_optional(
    config: &TrackerConfig,
) -> Result<(Option<WhatsAppSender>, Receiver<WhatsAppEvent>)> {
    let (events_tx, events_rx) = mpsc::channel();
    if config.pair_phone.is_none() {
        return Ok((None, events_rx));
    }
    let sender = WhatsAppSender::connect(config, events_tx)?;
    Ok((Some(sender), events_rx))
}

fn connect_telegram(config: &TrackerConfig) -> Result<Option<TelegramSender>> {
    match (
        config.telegram_bot_token.as_deref(),
        config.telegram_chat_id.as_deref(),
    ) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Ok(Some(TelegramSender::connect(config)?)),
        (Some(_), None) => bail!(
            "set TELEGRAM_CHAT_ID in {} after running `nse-tracker telegram-link`",
            config.root_dir.join(".env").display()
        ),
        (None, Some(_)) => bail!(
            "set TELEGRAM_BOT_TOKEN in {} before using Telegram",
            config.root_dir.join(".env").display()
        ),
    }
}

fn configured_sinks<'a>(
    whatsapp: Option<&'a WhatsAppSender>,
    telegram: Option<&'a TelegramSender>,
) -> Vec<&'a dyn AlertSink> {
    let mut sinks = Vec::new();
    if let Some(sender) = whatsapp {
        sinks.push(sender as &dyn AlertSink);
    }
    if let Some(sender) = telegram {
        sinks.push(sender as &dyn AlertSink);
    }
    sinks
}

fn link_whatsapp(config: &TrackerConfig) -> Result<()> {
    let (sender, events) = connect_optional(config)?;
    let Some(sender) = sender else {
        bail!(
            "set WHATSAPP_PAIR_PHONE in {} before linking",
            config.root_dir.join(".env").display()
        );
    };
    println!("Waiting for WhatsApp pairing or connection events. Press Ctrl-C to stop.");
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_secs(1)) {
            Ok(WhatsAppEvent::PairingCode {
                code,
                timeout_seconds,
            }) => {
                println!("Pairing code: {code} (valid for about {timeout_seconds} seconds)");
                println!("On your phone: WhatsApp → Linked devices → Link with phone number.");
            }
            Ok(WhatsAppEvent::Connected) => {
                println!("WhatsApp linked-device session is connected.");
                sender.shutdown();
                return Ok(());
            }
            Ok(WhatsAppEvent::LoggedOut) => {
                println!("WhatsApp reports that this device was logged out.")
            }
            Ok(WhatsAppEvent::PairingFailed { message }) => eprintln!("Pairing failed: {message}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("WhatsApp event stream ended"),
        }
    }
    sender.shutdown();
    bail!("timed out waiting for WhatsApp to connect")
}

fn link_telegram(config: &TrackerConfig) -> Result<()> {
    let token = config
        .telegram_bot_token
        .as_deref()
        .context("set TELEGRAM_BOT_TOKEN in .env before running `telegram-link`")?;
    let client = TelegramClient::new(token)?;
    let username = client
        .get_bot_username()?
        .map(|username| format!("@{username}"))
        .unwrap_or_else(|| "your Telegram bot".to_owned());
    println!("Connected to {username}.");
    println!("Open the bot in Telegram and send /start now; checking for 25 seconds...");
    let chat_ids = client.discover_private_chat_ids(25)?;
    if chat_ids.is_empty() {
        bail!(
            "no private chat was found; send /start to {username} and run `nse-tracker telegram-link` again"
        );
    }
    for chat_id in chat_ids {
        println!("Private chat ID found: {chat_id}");
        println!(
            "Set TELEGRAM_CHAT_ID={chat_id} in {}",
            config.root_dir.join(".env").display()
        );
    }
    Ok(())
}

fn run(config: TrackerConfig, mut send_existing: bool) -> Result<()> {
    let tracker = Tracker::new(config.clone())?;
    let (sender, events) = connect_optional(&config)?;
    let telegram = connect_telegram(&config)?;
    let sinks = configured_sinks(sender.as_ref(), telegram.as_ref());
    let shutdown_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not create the shutdown signal runtime")?;
    let mut stop_signals = StopSignals::new()?;
    println!(
        "Polling NSE every {} seconds. Watchlist: {}",
        config.poll_interval_seconds,
        config.stocks_file.display()
    );
    if sinks.is_empty() {
        println!(
            "No messaging destination is configured; alerts will queue in local SQLite state."
        );
    }

    loop {
        let cycle_start = Instant::now();
        print_queued_events(&events);
        if let Err(error) = match tracker.poll_once(&sinks, send_existing) {
            Ok(summary) => {
                print_summary(&summary);
                Ok(())
            }
            Err(error) => Err(error),
        } {
            eprintln!("Poll cycle failed: {error:#}");
        }
        send_existing = false;
        print_queued_events(&events);

        // Drain WhatsApp events during the interval, while also responding to
        // Ctrl-C and launchd's SIGTERM so the linked-device state is flushed.
        let interval = Duration::from_secs(config.poll_interval_seconds);
        let mut remaining = interval.saturating_sub(cycle_start.elapsed());
        while !remaining.is_zero() {
            let wait_for = remaining.min(Duration::from_millis(500));
            if stop_signals.wait(&shutdown_runtime, wait_for) {
                println!("Stopping tracker and flushing local WhatsApp state.");
                return Ok(());
            }
            print_queued_events(&events);
            remaining = interval.saturating_sub(cycle_start.elapsed());
        }
    }
}

#[cfg(unix)]
struct StopSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl StopSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .context("could not listen for Ctrl-C")?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("could not listen for SIGTERM")?,
        })
    }

    fn wait(&mut self, runtime: &tokio::runtime::Runtime, duration: Duration) -> bool {
        runtime.block_on(async {
            tokio::select! {
                _ = self.interrupt.recv() => true,
                _ = self.terminate.recv() => true,
                _ = tokio::time::sleep(duration) => false,
            }
        })
    }
}

#[cfg(not(unix))]
struct StopSignals;

#[cfg(not(unix))]
impl StopSignals {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn wait(&mut self, runtime: &tokio::runtime::Runtime, duration: Duration) -> bool {
        runtime.block_on(async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => true,
                _ = tokio::time::sleep(duration) => false,
            }
        })
    }
}

fn print_queued_events(events: &Receiver<WhatsAppEvent>) {
    while let Ok(event) = events.try_recv() {
        print_whatsapp_event(event);
    }
}

fn print_whatsapp_event(event: WhatsAppEvent) {
    match event {
        WhatsAppEvent::PairingCode {
            code,
            timeout_seconds,
        } => {
            println!("WhatsApp pairing code: {code} (valid for about {timeout_seconds} seconds)");
            println!(
                "Enter it on your phone under WhatsApp → Linked devices → Link with phone number."
            );
        }
        WhatsAppEvent::Connected => println!("WhatsApp linked-device session connected."),
        WhatsAppEvent::LoggedOut => eprintln!(
            "WhatsApp linked-device session logged out; run `nse-tracker whatsapp-link` to link again."
        ),
        WhatsAppEvent::PairingFailed { message } => eprintln!("WhatsApp pairing failed: {message}"),
    }
}

fn print_summary(summary: &nse_tracker::PollSummary) {
    println!(
        "Checked {} symbols; found {} new; baselined {}; sent {}; pending {}.",
        summary.symbols_checked,
        summary.new_announcements,
        summary.baselined_announcements,
        summary.messages_sent,
        summary.pending_alerts
    );
    for error in &summary.errors {
        eprintln!("Warning: {error}");
    }
}

fn manage_service(config: &TrackerConfig, command: ServiceCommand) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("the LaunchAgent service command currently supports macOS only");
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let launch_agents = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&launch_agents)
        .with_context(|| format!("could not create {}", launch_agents.display()))?;
    let plist = launch_agents.join("com.nse.tracker.plist");
    let label = "com.nse.tracker";
    let domain = format!("gui/{}", current_uid()?);

    match command {
        ServiceCommand::Install => {
            let _tracker = Tracker::new(config.clone())?;
            let executable = std::env::current_exe()
                .context("could not locate nse-tracker executable")?
                .canonicalize()?;
            let contents = launch_agent_plist(label, &executable, &config.root_dir);
            std::fs::write(&plist, contents)
                .with_context(|| format!("could not write {}", plist.display()))?;
            let _ = ProcessCommand::new("launchctl")
                .arg("bootout")
                .arg(format!("{domain}/{label}"))
                .status();
            let output = ProcessCommand::new("launchctl")
                .args(["bootstrap", &domain])
                .arg(&plist)
                .output()
                .context("could not run launchctl bootstrap")?;
            if !output.status.success() {
                let detail = String::from_utf8_lossy(&output.stderr);
                if !detail.contains("already bootstrapped") {
                    bail!(
                        "LaunchAgent was written to {}, but launchctl failed: {}",
                        plist.display(),
                        detail.trim()
                    );
                }
            }
            println!("Installed and started LaunchAgent at {}", plist.display());
            println!("Logs: {}", config.state_dir.join("launchd.log").display());
        }
        ServiceCommand::Uninstall => {
            let _ = ProcessCommand::new("launchctl")
                .args(["bootout", &domain])
                .arg(&plist)
                .status();
            if plist.exists() {
                std::fs::remove_file(&plist)
                    .with_context(|| format!("could not remove {}", plist.display()))?;
            }
            println!("Removed LaunchAgent {}", plist.display());
        }
    }
    Ok(())
}

fn current_uid() -> Result<String> {
    let output = ProcessCommand::new("id")
        .arg("-u")
        .output()
        .context("could not determine the current macOS user id")?;
    if !output.status.success() {
        bail!("id -u failed")
    }
    Ok(String::from_utf8(output.stdout)
        .context("id -u returned non-UTF-8 output")?
        .trim()
        .to_owned())
}

fn launch_agent_plist(label: &str, executable: &Path, config_dir: &Path) -> String {
    let arguments = [
        executable.to_string_lossy().into_owned(),
        "--config-dir".to_owned(),
        config_dir.to_string_lossy().into_owned(),
        "run".to_owned(),
    ];
    let arguments = arguments
        .iter()
        .map(|argument| format!("        <string>{}</string>\n", xml_escape(argument)))
        .collect::<String>();
    let log_path = config_dir.join(".nse-tracker/launchd.log");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n    <key>Label</key><string>{}</string>\n    <key>ProgramArguments</key>\n    <array>\n{}    </array>\n    <key>WorkingDirectory</key><string>{}</string>\n    <key>RunAtLoad</key><true/>\n    <key>KeepAlive</key><true/>\n    <key>StandardOutPath</key><string>{}</string>\n    <key>StandardErrorPath</key><string>{}</string>\n</dict>\n</plist>\n",
        xml_escape(label),
        arguments,
        xml_escape(&config_dir.to_string_lossy()),
        xml_escape(&log_path.to_string_lossy()),
        xml_escape(&log_path.to_string_lossy()),
    )
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
