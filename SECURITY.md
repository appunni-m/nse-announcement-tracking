# Security and account notes

## Local credentials

On macOS, the desktop panel stores WhatsApp phone numbers, the Telegram bot token, and selected chat ID in the `com.nse.tracker.messaging` generic-password items in macOS Keychain. Legacy `.env` messaging values are migrated into Keychain and blanked from the file. The CLI can still use `.env` for headless operation.

`.nse-tracker/whatsapp.sqlite3` contains the WhatsApp linked-device session and should be treated like a password. On Unix, the app creates `.nse-tracker` with owner-only permissions (`0700`). Do not commit the directory or upload it with support requests. Deleting the file removes this installation's local session; also unlink the device in WhatsApp under **Linked devices** to revoke its server-side access.

For CLI/headless use, `.env` may contain `TELEGRAM_BOT_TOKEN`, which controls the bot and must be kept secret. The file is ignored by Git. If a token leaks, revoke it with `/revoke` through Telegram's @BotFather. The tracker sends only to the configured chat and does not process incoming messages during normal polling. The widget calls `getUpdates` only during the user-started chat discovery flow.

The tracker stores announcement metadata, extracted text, Codex summaries, and delivery state in `.nse-tracker/state.sqlite3`. NSE filings are public, but the local history can show which symbols you monitor.

## WhatsApp protocol

This app links as a WhatsApp Web device through the Rust [`whatsapp-rust` crate](https://github.com/oxidezap/whatsapp-rust). It does not use Meta's hosted Cloud API or Hermes. The WhatsApp Web protocol client is unofficial. WhatsApp warns that linking through unofficial apps or websites can put an account at risk, including a temporary or permanent ban; review [WhatsApp's linked-device guidance](https://faq.whatsapp.com/378279804439436/) before pairing. Use this route only if you accept that risk.

The app pins crate version 0.7.0. Its API is pre-1.0 and may change between releases. Review the upstream project and its current status before upgrading.

## Downloads

NSE attachment URLs must use HTTPS and an `nseindia.com` hostname. Redirects to other hosts are stopped, downloads are size-limited, and the response must begin with a PDF signature before local text extraction. When `CODEX_PDF_ANALYSIS=true` (the default), extracted PDF text is passed to the installed Codex CLI. The CLI process runs from a private temporary directory, with user configuration ignored, a read-only sandbox, and a 120-second timeout. The Codex CLI is an open-source local harness, but model access is separate; the extracted text is sent to the CLI's model service and uses the user's Codex access. Saved CLI authentication remains available, but custom model-provider and MCP settings in `config.toml` are not loaded. Set `CODEX_PDF_ANALYSIS=false` to avoid sending PDF text to Codex; local text extraction still runs. Successful summaries are stored in the announcement database. Extracted text and/or summaries are also sent to configured messaging destinations. For Telegram, that means the configured chat through Telegram's HTTPS Bot API.

## Reporting a security issue

There is no project-specific security contact published in this repository yet. If this project is hosted on GitHub and private vulnerability reporting is enabled, use that channel. Otherwise, contact the repository maintainer privately through the hosting service before sharing an active exploit publicly. Do not include linked-device databases or phone numbers in reports.
