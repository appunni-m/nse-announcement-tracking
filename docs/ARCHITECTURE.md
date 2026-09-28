# Architecture

The repository contains a Rust library and two application packages:

- The root package exports `nse_tracker`, the reusable polling and storage API, and the `nse-tracker` CLI.
- `crates/widget` contains `nse-tracker-widget`, a compact eframe desktop app built on the library.

The CLI, widget, and library share one `TrackerConfig`, one JSON watchlist, and local SQLite databases for announcements and WhatsApp session state. UI code calls into the library; the widget's worker thread owns blocking NSE and PDF work so the window remains responsive. Telegram chat discovery runs on a separate setup thread so its long-poll does not pause NSE polling.

## Poll cycle

1. Read and validate `stocks.json`.
2. Warm up an NSE cookie-backed HTTP client once per process, then request the corporate-announcement endpoint for each symbol with a configurable delay.
3. Store new `(symbol, seq_id)` entries. On the first successful poll for each symbol, mark the current lookback set as a silent baseline unless the caller asked to send existing items.
4. Download eligible NSE PDF attachments with a byte limit and extract text locally.
5. If enabled, queue extracted text for a single background Codex worker. It invokes the CLI in a read-only temporary workspace and saves the structured summary or one-time failure state in SQLite. Continuous NSE polling does not wait for analysis; alerts do.
6. Send queued messages through each configured `AlertSink`. SQLite records WhatsApp and Telegram delivery separately; if one destination fails, retries target only that destination.

NSE or PDF errors are retained in the polling summary. Failed PDF extraction stays pending for up to three polling cycles and then produces an alert with the NSE summary and attachment URL. Delivery errors leave the message pending.

## Data and network boundaries

Announcements come from NSE's public website and attachments are accepted only from HTTPS hosts equal to `nseindia.com` or below that domain. PDF text extraction is local. With Codex analysis enabled, that extracted text is passed to the user's authenticated Codex CLI and sent to the model service used by that CLI; the harness is local, but model access is separate. The run uses an isolated temporary directory, ignores user Codex configuration, and requests a read-only sandbox. Saved CLI authentication remains available, but custom model-provider and MCP settings in `config.toml` are not loaded. The summary and source text are stored in the local database. When Telegram is configured, alert text is also sent to Telegram's hosted Bot API over HTTPS. This project does not run its own server.

WhatsApp pairing, connection, and delivery are handled by the pinned [`whatsapp-rust` crate](https://github.com/oxidezap/whatsapp-rust), which implements the WhatsApp Web protocol. This path is unofficial and may break when WhatsApp changes its protocol or applies account restrictions. WhatsApp warns that linking through unofficial apps may put an account at risk, including bans; see [Security](../SECURITY.md) before pairing.

Telegram delivery is implemented in `src/telegram.rs` using the project's existing `reqwest` and `serde` dependencies. The widget calls `getMe` and `getUpdates` only after the user starts chat discovery; the CLI also retains its explicit setup command. The widget stores the bot token and selected chat ID in macOS Keychain, while headless CLI configuration continues to read `.env`. The token is redacted from `TrackerConfig` debug output. The bot does not process inbound commands during normal polling.

Codex summaries are implemented in `src/codex.rs` by invoking `codex exec --output-schema` and reading the structured final response on one background thread. The executable is resolved from `CODEX_CLI_PATH`, `PATH`, or common per-user macOS install locations. The worker persists success or failure per announcement to avoid repeating model calls on every poll. The one-shot CLI command waits for its queue to drain; continuous pollers do not. It does not require a Rust Codex SDK dependency or a separate API client.

## Dependency policy

The project uses established Rust crates for HTTP, JSON, SQLite, PDF extraction, command parsing, async runtime, WhatsApp Web, and the desktop panel. `Cargo.lock` pins the resolved graph. The WhatsApp crate is pinned to `0.7.0` because its API is pre-1.0. Its optional nightly SIMD feature is disabled so the application can build on stable Rust.
