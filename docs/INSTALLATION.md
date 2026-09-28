# Installation

This guide builds NSE Tracker from source on macOS. The desktop panel and LaunchAgent are macOS-targeted in this release; the Rust library and CLI are written without macOS-specific polling code.

## Requirements

- macOS and a supported Rust toolchain, version 1.95 or newer.
- Network access to NSE and whichever messaging channel you configure.
- An installed and authenticated Codex CLI if you want PDF summaries (enabled by default).
- For WhatsApp: an account that can link another device.
- For Telegram: a Telegram account to create a bot and start a private chat with it.

Cargo downloads the Rust crates listed in `Cargo.lock`. The app does not install Codex CLI, Hermes, Node.js, Python, a browser, a database server, or a Meta Cloud messaging service. Install and sign in to the [Codex CLI](https://developers.openai.com/codex/cli) yourself if you want the default PDF-analysis behavior. The worker calls `codex exec` using the CLI's available authentication; it ignores user `config.toml` settings, including custom model-provider and MCP configuration. Set `CODEX_PDF_ANALYSIS=false` to omit this step. SQLite is compiled through the Rust dependency graph. Telegram uses the existing Rust HTTP client dependency to call Telegram's hosted Bot API; no Telegram-specific crate or extra daemon is installed. The macOS panel stores messaging settings in macOS Keychain. WhatsApp's linked-device protocol session remains in the private local SQLite file required by its client library.

## Build and install the commands

From the repository root:

```sh
cargo install --path crates/widget --locked
```

Install the CLI too only if you want headless operation or service management:

```sh
cargo install --path . --locked
```

Cargo places the commands in its binary directory, normally `~/.cargo/bin`. Ensure that directory is on `PATH`.

To build without installing:

```sh
cargo build --release --workspace --locked
./target/release/nse-tracker --help
./target/release/nse-tracker-widget
```

## Configure WhatsApp in the panel

Launch `nse-tracker-widget`, open **Set up alerts**, enter the linking number with its country code, and choose **Save & connect WhatsApp**. Leave the alert recipient blank to send to the linked number's self-chat, or enter a different recipient with its country code. When the pairing code appears, enter it on your phone under **WhatsApp → Linked devices → Link with phone number**. The panel reports when the linked device is connected and offers a user-triggered test message. Read [Security and account notes](../SECURITY.md) before using this unofficial linked-device client.

The panel stores the numbers in macOS Keychain. The WhatsApp linked-device session is held in `.nse-tracker/whatsapp.sqlite3` by the client library; keep that private folder protected. If WhatsApp logs the device out, open **Set up alerts** and choose **Save & connect WhatsApp** again.

## Configure Telegram in the panel

Create a bot using Telegram's official [@BotFather](https://t.me/BotFather): send `/newbot`, follow the prompts, and keep the resulting token private. Open **Set up alerts** in the panel, paste the token, then open the new bot in Telegram and send `/start`. Return to the panel and choose **Find my Telegram chats**. Select the private chat and click **Enable selected chat**. The panel verifies the bot token and stores it and the selected numeric destination in macOS Keychain. Use **Send test** to verify delivery.

The chat search uses Telegram `getUpdates` once during setup; it does not read incoming messages during normal tracking. Telegram's `getUpdates` method is unavailable if a webhook is configured for the bot. If no chat appears, send `/start` to the bot and search again.

Telegram hosts the Bot API, and standard bot messages are free within Telegram's platform limits. The client is implemented in Rust within this repository and makes HTTPS requests directly to `api.telegram.org`. No Hermes gateway or Telegram wrapper dependency is used. Telegram recommends no more than one message per second in a single chat; the sender spaces a backlog accordingly.

## Use both channels

Complete both in-app setup flows. The poller sends each alert through both destinations and records delivery for each channel independently. A failed channel retries on the next poll without resending to a channel that already accepted that alert.

## Headless and CLI operation

The widget is the recommended setup for desktop use. `.env` messaging values remain available for the CLI and headless installations; the widget migrates existing channel values to Keychain and clears them from `.env`.

```sh
nse-tracker init
nse-tracker stock list
nse-tracker poll-once
nse-tracker run
```

The first poll records existing announcements in the lookback period silently. For a one-time initial import that also sends those filings, use:

```sh
nse-tracker poll-once --send-existing
```

The default polling interval is 60 seconds. `NSE_POLL_INTERVAL_SECONDS` cannot be set below 60. See [Usage](USAGE.md) for all settings and watchlist operations.

When Codex analysis is enabled, the worker extracts text from each new PDF and queues one `codex exec` call. A single background Rust worker processes filings sequentially so continuous NSE polling can continue; alerts remain pending until analysis finishes. The one-shot `poll-once` command waits for its queued analyses before exiting. Each CLI call uses an isolated temporary directory, ignores Codex user configuration, uses a read-only sandbox, and stops after 120 seconds. Extracted document text is sent through the model service used by Codex and consumes the user's Codex access. To set an explicit executable path for launchd or a custom install, set `CODEX_CLI_PATH` in `.env`, for example `/Users/your-name/.local/bin/codex`. If Codex is unavailable, alerts continue with local PDF text.

## Start at login with launchd

Install the command first, then run the installer from the directory containing `.env` and `stocks.json`:

```sh
nse-tracker service install
```

The command writes `~/Library/LaunchAgents/com.nse.tracker.plist`, starts it for the current macOS user, and configures launchd to keep polling alive. Logs are written to `.nse-tracker/launchd.log` under the config directory.

If the config files live elsewhere, pass their directory explicitly:

```sh
nse-tracker --config-dir "$HOME/stock-automation" service install
```

Remove the LaunchAgent with:

```sh
nse-tracker service uninstall
```

Do not run the foreground `nse-tracker run` command or the desktop panel at the same time as the LaunchAgent. They share the same SQLite session and polling state.

## Desktop panel

Run it from the configuration directory:

```sh
nse-tracker-widget
```

Or choose a different directory:

```sh
nse-tracker-widget --config-dir "$HOME/stock-automation"
```

The panel runs polling on a background Rust thread and provides symbol add/remove controls, in-app messaging setup, WhatsApp connection state, and recent filings. WhatsApp pairing and Telegram bot/chat selection happen without leaving the panel; credentials are saved to macOS Keychain. Telegram's one-time chat search runs in the background while the tracker remains responsive. The panel and a polling CLI or LaunchAgent must not run at the same time.

## Remove local data

Uninstalling the binaries does not erase files. Stop the LaunchAgent, then remove `.nse-tracker` from the chosen config directory to delete local announcement history and the WhatsApp linked-device database. Messaging values saved by the widget are in macOS Keychain; remove the `com.nse.tracker.messaging` generic-password entries from Keychain Access if you want to erase them. Revoke a Telegram bot token with `/revoke` through @BotFather. Keep or remove `stocks.json` and `.env` separately according to your preference.
