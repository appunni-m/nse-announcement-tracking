# Usage and operations

## Watchlist

Each watchlist entry is an NSE symbol and market API route:

```json
[
  { "symbol": "ALPEXSOLAR", "market_api_type": "sme" },
  { "symbol": "RELIANCE", "market_api_type": "equities" }
]
```

Use the CLI to add, change, list, and remove entries:

```sh
nse-tracker stock add RELIANCE --market equities
nse-tracker stock add ALPEXSOLAR --market sme
nse-tracker stock list
nse-tracker stock remove RELIANCE
```

Adding an existing symbol updates its market route. The tracker reloads `stocks.json` each cycle, so edits made with the CLI, widget, or a text editor take effect on the next poll. Symbols are normalized to uppercase and accept letters, digits, `&`, `-`, and `.`.

## Polling and delivery

```sh
nse-tracker poll-once
nse-tracker run
```

The poller requests corporate announcements for the previous `NSE_LOOKBACK_DAYS` through today using the NSE `getCorporateAnnouncement` endpoint. The rolling window handles delayed API updates; stored `(symbol, seq_id)` pairs suppress duplicates.

For a symbol's first successful poll, current announcements are stored as `baseline` and no messages are sent. This avoids sending the whole lookback window when first setting up. `poll-once --send-existing` changes the first poll to queue those announcements. After the first poll, newly discovered filings enter the `pending` queue.

For each filing with a PDF, the app downloads from an HTTPS NSE subdomain, checks the size and PDF signature, then extracts text locally. A failed extraction is retried on later polling cycles, up to three attempts. Scanned PDFs without embedded text cannot be analyzed. After successful extraction, the worker runs `codex exec` once to identify the event type, apparent importance, key facts, and reported figures. The result is saved in SQLite and used in the alert and desktop panel. If Codex is unavailable or fails, the alert still sends with extracted PDF text. After the final PDF extraction failure, the alert includes the NSE text summary, error note, and attachment link. Filings without a PDF are sent using the NSE summary. Each enabled channel has a separate delivery flag in SQLite; a failed WhatsApp or Telegram send retries on the next cycle without resending to a channel that already succeeded.

Telegram uses the Bot API from Rust over HTTPS. The widget reads the bot token and selected numeric chat ID from macOS Keychain; the CLI reads them from `.env`. Long PDF text is clipped to fit Telegram's 4,096-character limit. The sender spaces messages at least 1.1 seconds apart to stay within the usual per-chat rate. A Telegram rate-limit or network error leaves that channel pending for the next poll. During normal tracking the app only sends messages and does not poll bot updates.

Run only one polling process at a time: choose `nse-tracker run`, the desktop panel, or the LaunchAgent. The CLI and panel schedule the next cycle from the current cycle's start, so they wait only for the remaining part of the configured interval. A slow NSE request or PDF download/extraction can still make the actual start-to-start time longer than 60 seconds. Codex uses one background worker and does not block continuous polling; if several filings queue up, their alerts wait for sequential analysis. The one-shot `nse-tracker poll-once` command waits for its Codex work before exiting.

## Configuration

The CLI reads `.env` from the config directory. Process environment variables take precedence. The parser supports blank lines, `#` comments, and `KEY=value`; quote marks around values are stripped. The macOS widget uses **Set up alerts** and saves messaging secrets to Keychain. When it finds legacy messaging values in `.env`, it migrates them to Keychain and blanks those entries. Codex CLI analysis is enabled by default. It uses the local `codex` executable and the account configured for that CLI. Install and sign in to the [Codex CLI](https://developers.openai.com/codex/cli) separately; the Rust build does not install it.

| Setting | Default | Accepted range or meaning |
| --- | --- | --- |
| `WHATSAPP_PAIR_PHONE` | unset | Country code + phone digits, 8–15 digits. Enables WhatsApp linking. |
| `WHATSAPP_ALERT_PHONE` | paired number | Country code + phone digits. Blank means self-chat. |
| `TELEGRAM_BOT_TOKEN` | unset | Token issued by @BotFather. Keep it private. |
| `TELEGRAM_CHAT_ID` | unset | Signed numeric chat ID returned by `nse-tracker telegram-link`. |
| `NSE_POLL_INTERVAL_SECONDS` | `60` | 60 to 86,400 seconds. |
| `NSE_LOOKBACK_DAYS` | `7` | 1 to 30 calendar days. |
| `NSE_REQUEST_DELAY_MS` | `300` | 0 to 60,000 milliseconds between symbols. |
| `PDF_TEXT_LIMIT_CHARS` | `2200` | 100 to 10,000 extracted characters stored, analyzed by Codex, and available for alerts. |
| `MAX_PDF_BYTES` | `25000000` | 100,000 to 100,000,000 bytes. |
| `CODEX_PDF_ANALYSIS` | `true` | Set `false` to skip Codex and use extracted PDF text in alerts. |
| `CODEX_CLI_PATH` | auto-detect | Optional path to `codex`, useful when launchd has a limited `PATH`. |

Codex analysis sends extracted PDF text to the model service used by the Codex CLI and consumes the user's Codex access. One Rust background worker processes analyses sequentially; each CLI call runs with user config ignored, a read-only sandbox, an isolated temporary working directory, and a 120-second timeout. Saved CLI authentication remains available, while custom model-provider and MCP settings in `config.toml` are not loaded. A successful result or failure is stored once per filing, so polling does not repeatedly analyze the same PDF. See [Security](../SECURITY.md) for the data boundary.

Set a different config directory with `--config-dir PATH`:

```sh
nse-tracker --config-dir "$HOME/stock-automation" stock list
nse-tracker --config-dir "$HOME/stock-automation" run
```

## Desktop panel

`nse-tracker-widget` displays current status, watchlist, WhatsApp pairing code, and recent announcements, including Codex summaries when available. Add or remove symbols from the panel; each edit updates the shared `stocks.json`. **Set up alerts** contains in-app WhatsApp pairing and Telegram token/chat setup, plus user-triggered test messages and channel disable actions. Telegram chat discovery runs off the polling thread. The widget shares the announcement and WhatsApp session databases with the CLI, so do not run both pollers at once.

If a symbol is rejected, check the market route (`equities` for main-board listings, `sme` for SME listings). If WhatsApp shows disconnected, verify network access and choose **Save & connect WhatsApp** again after a logout. For Telegram setup errors, verify the bot token, send `/start` to the bot, and run **Find my Telegram chats** again. `getUpdates` cannot run when a webhook is configured for the bot.

## State files

The tracker creates the following under the config directory:

| Path | Contents |
| --- | --- |
| `stocks.json` | Current symbol list. |
| `.nse-tracker/state.sqlite3` | Announcement keys, extracted PDF text, Codex summary/error state, retry counts, and alert status. |
| `.nse-tracker/whatsapp.sqlite3` | WhatsApp linked-device session credentials. |
| `.nse-tracker/launchd.log` | macOS LaunchAgent standard output and error. |

Back up the watchlist and configuration separately. Treat the WhatsApp database as a password-equivalent secret. The announcement database contains locally extracted public filings and message delivery state.
