# NSE Tracker

NSE Tracker is a Rust library, command-line app, and compact macOS desktop panel for following NSE corporate announcements. It polls a configurable symbol list, downloads NSE PDF attachments, extracts their text, asks the installed Codex CLI to summarize each new PDF once, and keeps alerts queued until each configured delivery channel succeeds.

Alerts can go to WhatsApp, Telegram, or both. WhatsApp runs locally through an unofficial WhatsApp Web client; Telegram uses a small in-process Rust client that calls Telegram's HTTPS Bot API directly. No Hermes gateway, Telegram wrapper crate, or Meta Cloud API is used. WhatsApp warns that unofficial linked-device apps can put an account at risk, including bans. See [Security and account notes](SECURITY.md) before pairing or configuring credentials.

## Quick start on macOS

Requirements: macOS, stable Rust 1.95 or newer, and an internet connection. Install and sign in to the [Codex CLI](https://developers.openai.com/codex/cli) for PDF summaries, or disable them with `CODEX_PDF_ANALYSIS=false`. Rust dependencies are fetched from Cargo when building. The project bundles SQLite through its Rust dependency; it does not require a separately installed database, Hermes, Node.js, Python, or a paid messaging provider. Telegram hosts the Bot API; standard bot messages are free within Telegram's platform limits.

From the repository root, install and launch the desktop panel:

```sh
cargo install --path crates/widget --locked
nse-tracker-widget
```

Use **Set up alerts** in the panel to connect WhatsApp, Telegram, or both. Pairing, chat selection, disabling, and optional test messages are all available in the panel; channel secrets are stored in macOS Keychain. The CLI remains available for headless installs and automation. See [Installation](docs/INSTALLATION.md) for the full setup.

For WhatsApp pairing, open WhatsApp on your phone and choose **Linked devices → Link with phone number**, then enter the code shown in the panel. The linked-device session is stored under `.nse-tracker/whatsapp.sqlite3`.

For headless polling at login, install the CLI with `cargo install --path . --locked`, stop the desktop panel, then install the per-user macOS LaunchAgent from the project directory:

```sh
nse-tracker service install
```

### Telegram setup in the panel

Create a bot with Telegram's official [@BotFather](https://t.me/BotFather) using `/newbot`. In the panel, open **Set up alerts**, paste the token, open the new bot in Telegram and send `/start`, then select **Find my Telegram chats**. Choose the private destination shown and enable it. The bot token and selected chat are stored in macOS Keychain. Headless CLI users can still configure `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` through `.env`.

Telegram's [Bot API](https://core.telegram.org/bots/api) accepts text messages up to 4,096 characters. PDF contents are extracted locally, clipped to fit an alert, and sent as text with the NSE PDF link. The bot does not upload the PDF or process incoming messages during normal polling. Telegram recommends avoiding more than one message per second in a single chat; the client spaces queued messages accordingly, and Telegram may still reject sends temporarily.

See [Installation](docs/INSTALLATION.md) for alternate build commands, configuration, and service removal.

## Manage symbols

The included `stocks.json` starts with `ALPEXSOLAR` on the SME route. Add or remove symbols while the app runs; the next polling cycle reloads the file.

```sh
nse-tracker stock add RELIANCE --market equities
nse-tracker stock add ALPEXSOLAR --market sme
nse-tracker stock list
nse-tracker stock remove RELIANCE
```

You can also add and remove symbols in the desktop panel. Main-board symbols use `equities`; SME symbols use `sme`.

## What gets stored

- `stocks.json`: editable NSE symbol list.
- `.nse-tracker/state.sqlite3`: seen announcements, extracted PDF text, Codex summary state, and delivery status.
- `.nse-tracker/whatsapp.sqlite3`: WhatsApp linked-device session credentials.
- macOS Keychain service `com.nse.tracker.messaging`: widget phone numbers, Telegram bot token, and selected chat ID.
- `.nse-tracker/launchd.log`: output from the optional macOS LaunchAgent.

## Codex PDF summaries

The worker extracts PDF text locally, then queues one `codex exec` analysis for each newly queued PDF that yields text. One background Rust worker analyzes filings sequentially, so Codex latency does not block the next NSE poll. Alerts remain queued until the summary or a failure is saved; a growing analysis backlog can delay delivery. Codex returns a structured event summary, key facts, and reported figures, stored in SQLite and shown in alerts and the desktop panel. The one-shot `poll-once` command waits for its Codex work before exiting. The worker uses the authentication available to the installed Codex CLI; it does not add a separate API client or credential. Cargo does not install the CLI. The extracted text is sent to the model service used by Codex, so it consumes the user's Codex access. Codex CLI is an open-source harness, while model access is separate ([OpenAI's Codex platform overview](https://developers.openai.com/blog/codex-as-a-platform)).

Analysis is enabled by default. Set `CODEX_PDF_ANALYSIS=false` in `.env` to skip it and send the extracted text instead. Set `CODEX_CLI_PATH` if the executable is outside `PATH`; this is useful for launchd, which has a limited environment. Codex runs in an isolated temporary directory with user configuration ignored, writes disabled, and a 120-second timeout; saved CLI authentication remains available, while custom model-provider and MCP settings in `config.toml` are not loaded. Each filing is analyzed at most once; if Codex is unavailable or returns an error, the alert still goes out with extracted PDF text.

The first poll for each symbol stores current filings as a baseline without sending old alerts. Use `nse-tracker poll-once --send-existing` if you want filings in the configured lookback window sent during initial setup. Failed PDF extraction is retried across polling cycles up to three times; a filing is still sent with its NSE summary and PDF link after the final failed attempt. Delivery is recorded separately for WhatsApp and Telegram, so a successful channel is not sent the same queued filing again while another channel retries.

## Library

The root Rust package exposes `Tracker`, `TrackerConfig`, watchlist types, recent announcement records, `AlertSink`, and the WhatsApp and Telegram clients. Run `cargo doc --open` to read the generated API documentation. See [Usage](docs/USAGE.md) for configuration and the polling model.

## Desktop panel

The separate `nse-tracker-widget` package provides a compact native Rust window for watchlist management, in-app WhatsApp pairing, Telegram chat selection, connection status, and recent announcements. It uses the same library, watchlist, and local databases as the CLI. It is a regular desktop window; it is not an Apple Notification Center WidgetKit extension. The first supported desktop target is macOS. Windows and Linux builds are future work.

```sh
nse-tracker-widget
```

## Limitations

- NSE may change the announcement API, response fields, anti-bot checks, or PDF hostnames. The app requests NSE once per symbol per cycle and adds a configurable delay between symbols. It first loads NSE's corporate-filings page and keeps its cookies for the API request; the site root can return 403 even when this page and its API work. If NSE also rejects the filings page or the refreshed API request, the block may be based on the client or network session: `reqwest` does not execute browser JavaScript or share a browser's TLS fingerprint, so copying browser headers cannot guarantee access.
- Announcements are checked against a rolling lookback window. Local SQLite sequence IDs suppress duplicates.
- The WhatsApp Web protocol is unofficial and the [`whatsapp-rust` crate](https://github.com/oxidezap/whatsapp-rust) is pre-1.0. Its behavior and API can change between releases.
- Telegram delivery requires a bot token and chat ID; the panel's one-time private-chat search uses `getUpdates`, which Telegram does not allow while a webhook is configured for that bot.
- The widget and CLI share announcement and WhatsApp session databases. Run only one polling process at a time.
- macOS is the supported desktop target in this initial version.

## Project docs

- [Installation](docs/INSTALLATION.md)
- [Usage and operations](docs/USAGE.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)

## License

MIT. See [LICENSE](LICENSE).
