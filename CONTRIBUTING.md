# Contributing

Contributions are welcome. Keep the tracker, CLI, and desktop panel in Rust, preserve the local-first data model, and document user-visible behavior changes.

## Development setup

Install stable Rust 1.95 or newer, then run these commands from the repository root:

```sh
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
```

The workspace includes the root library and CLI plus `crates/widget`. Building the widget uses eframe's native window dependencies and is intended for macOS in this version. Cargo resolves packages from the checked-in `Cargo.lock`.

## Change guidelines

- Keep network polling, persistence, PDF extraction, and message formatting in the root library so both front ends share behavior.
- Do not put WhatsApp credentials, `.env` values, local SQLite files, or personal stock lists in a patch.
- Keep NSE request behavior polite; preserve the delay setting and rolling lookback.
- Add or update Rust API docs for public library changes.
- Update `README.md` or the focused guide in `docs/` when commands, settings, state files, or failure behavior change.
- Pin pre-1.0 integrations and explain their compatibility and account implications.

## Pull requests

Include a concise problem statement, behavior change, commands used to validate the patch, and any platform limitations. For parser or protocol changes, include a sanitized public example that contains no personal identifiers or WhatsApp session data.

## License

By contributing, you agree that your contribution is released under the repository's MIT license.
