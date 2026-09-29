# Castle

A native agent harness with a GPUI desktop application.

The workspace keeps three internal layers: desktop → harness → SDK. See the
[core architecture](docs/architecture/overview.md). The crates are not published independently.

## Install

Download the DMG (macOS), Setup EXE (Windows), or AppImage/DEB (Linux) from
[GitHub Releases](https://github.com/shenxiangzhuang/castle/releases).

## Run

Launch the downloaded desktop app, then configure OpenAI or DeepSeek in **Settings → Models**.
From a source checkout:

```bash
just macos-run                         # macOS app bundle
cargo run -p desktop --release # Linux or Windows desktop app
```

Desktop details live in [crates/desktop/README.md](crates/desktop/README.md).
Current prerelease desktop installers are unsigned and may trigger operating-system warnings.

## Develop

Source builds require Rust 1.97 or newer.

```bash
just pre-push # Daily formatting, lint, and test checks
just qa       # Also build optimized release binaries
```

Project architecture and development workflows live in [docs/README.md](docs/README.md).

## License

[Apache-2.0](LICENSE)

## Acknowledgements

Inspired by [pi](https://github.com/badlogic/pi-mono) and
[DeepSeek Harness](https://github.com/deepseek-ai/DeepSeek-Harness).
