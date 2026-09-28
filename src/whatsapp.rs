use crate::alert::{AlertSink, DeliveryChannel};
use crate::settings::{TrackerConfig, ensure_private_state_dir};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use tokio::runtime::{Builder, Runtime};
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::{Bot, BotHandle, Client, Jid, SqliteStore};

/// Lifecycle events emitted while the WhatsApp Web client connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppEvent {
    /// WhatsApp issued an eight-character pairing code for the mobile app.
    PairingCode { code: String, timeout_seconds: u64 },
    /// The linked-device session is connected and can send messages.
    Connected,
    /// WhatsApp logged out the linked device; it must be paired again.
    LoggedOut,
    /// WhatsApp rejected a pairing-code request or could not complete linking.
    PairingFailed { message: String },
}

/// A Rust WhatsApp Web session used to send stock alerts.
///
/// This client runs in-process and stores its linked-device session in the
/// configured SQLite file. Keep it alive while polling; dropping it gracefully
/// shuts down the background connection and flushes local state.
pub struct WhatsAppSender {
    runtime: Runtime,
    handle: Option<BotHandle>,
    client: Arc<Client>,
    destination: Jid,
    connected: Arc<AtomicBool>,
}

impl WhatsAppSender {
    /// Start the local WhatsApp Web client and request a pairing code if needed.
    ///
    /// `events` receives pairing and connection changes. On first use, enter the
    /// pairing code in WhatsApp under **Linked devices → Link with phone number**.
    /// The pair and alert numbers come from [`TrackerConfig`].
    ///
    /// # Errors
    /// Returns an error if the phone numbers are missing, the local session
    /// database cannot be opened, or the WhatsApp client cannot be constructed.
    pub fn connect(config: &TrackerConfig, events: Sender<WhatsAppEvent>) -> Result<Self> {
        let pair_phone = config
            .pair_phone
            .as_deref()
            .context("configure a phone number before linking a WhatsApp device")?;
        let alert_phone = config
            .alert_phone
            .as_deref()
            .context("configure a WhatsApp alert recipient")?;

        ensure_private_state_dir(&config.state_dir)?;

        let runtime = Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("could not create the Tokio runtime for WhatsApp")?;
        let backend = runtime
            .block_on(SqliteStore::new(
                &config.whatsapp_database_file.to_string_lossy(),
            ))
            .context("could not open WhatsApp linked-device storage")?;

        let connected = Arc::new(AtomicBool::new(false));
        let mut builder = Bot::builder()
            .with_backend(backend)
            .with_pair_code(PairCodeOptions {
                phone_number: pair_phone.to_owned(),
                ..Default::default()
            });

        let code_events = events.clone();
        builder = builder.on_pair_code(move |code, timeout| {
            let _ = code_events.send(WhatsAppEvent::PairingCode {
                code,
                timeout_seconds: timeout.as_secs(),
            });
            async {}
        });

        let pair_error_events = events.clone();
        builder = builder.on_pair_code_error(move |error, _client| {
            let _ = pair_error_events.send(WhatsAppEvent::PairingFailed {
                message: format!("{error:?}"),
            });
            async {}
        });

        let connected_flag = Arc::clone(&connected);
        let connected_events = events.clone();
        builder = builder.on_connected(move |_client| {
            connected_flag.store(true, Ordering::SeqCst);
            let _ = connected_events.send(WhatsAppEvent::Connected);
            async {}
        });

        let logged_out_flag = Arc::clone(&connected);
        builder = builder.on_logged_out(move |_info| {
            logged_out_flag.store(false, Ordering::SeqCst);
            let _ = events.send(WhatsAppEvent::LoggedOut);
            async {}
        });

        let bot = runtime
            .block_on(builder.build())
            .context("could not build the WhatsApp client")?;
        let handle = bot.spawn();
        let client = handle.client();

        Ok(Self {
            runtime,
            handle: Some(handle),
            client,
            destination: Jid::pn(alert_phone),
            connected,
        })
    }

    /// Whether the linked device is currently connected to WhatsApp.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Send one text message to the configured WhatsApp alert recipient.
    ///
    /// This performs network I/O and blocks the caller until the client returns.
    /// Call it from a worker thread, not from a GUI event handler or async runtime.
    ///
    /// # Errors
    /// Returns an error if the session is offline or WhatsApp rejects the send.
    pub fn send_text(&self, message: &str) -> Result<()> {
        if !self.is_connected() {
            bail!("WhatsApp linked-device session is not connected");
        }
        self.runtime
            .block_on(
                self.client
                    .send_text(self.destination.clone(), message.to_owned()),
            )
            .context("WhatsApp message send failed")?;
        Ok(())
    }

    /// Gracefully stop the client and flush its local linked-device state.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.runtime.block_on(handle.shutdown());
        }
    }
}

impl AlertSink for WhatsAppSender {
    fn channel(&self) -> DeliveryChannel {
        DeliveryChannel::WhatsApp
    }

    fn send_text(&self, message: &str) -> Result<()> {
        WhatsAppSender::send_text(self, message)
    }
}

impl Drop for WhatsAppSender {
    fn drop(&mut self) {
        self.stop();
    }
}
