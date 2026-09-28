use anyhow::Result;

/// A configured destination for corporate-announcement text alerts.
pub trait AlertSink {
    /// Return the delivery channel used by this destination.
    fn channel(&self) -> DeliveryChannel;

    /// Send one alert message.
    fn send_text(&self, message: &str) -> Result<()>;
}

/// Supported alert delivery channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryChannel {
    /// A locally linked WhatsApp Web device.
    WhatsApp,
    /// A Telegram bot chat using the HTTPS Bot API.
    Telegram,
}
