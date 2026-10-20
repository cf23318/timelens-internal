# TimeLens Internal

An internal desktop adaptation of [gitxuzhefeng/timelines](https://github.com/gitxuzhefeng/timelines) v0.17.5 (f8e7107), under the [MIT license](LICENSE). Current internal version: **0.17.6**. See the [Chinese README](README.md) for full configuration and acceptance details.

- Chinese and light appearance by default; collection starts manually. Pause/resume is available, and restarting leaves collection paused.
- Periodic WebP screenshots (12-second default attempt interval), foreground-window events, local timeline and replay. Replay works with OCR disabled; optional OCR runs locally. This is sampled screenshots, not continuous video.
- Two workdays default to 16 accumulated hours. The foreground display is captured by default; all displays are optional. Windows input counters are not implemented.
- Foreground privacy rules block listed apps, sensitive titles and banking domains. Unreadable browser addresses block screenshots. Background content may still appear; there is no pixel masking. New titles are redacted; old records are not rewritten automatically.
- Upload is off until configured and explicitly confirmed. Each original WebP is sent to an internal HTTPS receiver with required metadata; the activity database stays local. There is no new age archive, automatic local deletion or 30-day expiry. Restarting disables upload again.
- External AI and online updates are disabled. Upstream AI reports and upstream download links do not describe this internal build. Example endpoints do not prove deployment.

## Builds and contributions

Use a dedicated branch and PR. [Build](https://github.com/cf23318/timelens-internal/actions/workflows/build.yml) automatically tests and builds PRs and main for Windows and macOS. Download the matching successful run's `TimeLens-windows` or `TimeLens-macos` artifact (30-day retention), and verify `BUILD-INFO.json` and `SHA256SUMS`. Windows includes NSIS and standalone executables; macOS includes DMG. Artifacts may require GitHub sign-in. Actions stays enabled as the standard build path.

Before merging desktop behavior changes, test the affected platform with authorized fixtures and upload off. Merge only after checks and the required authorization. Releases require separate approval; a `v*` tag produces a draft using the same build workflow. Unsigned/unnotarized packages are for controlled testing.

## Development

Node.js 22+, Rust stable, and [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) are required. Windows also needs MSVC and WebView2.

```sh
git clone https://github.com/cf23318/timelens-internal.git
cd timelens-internal/project
npm ci
npm run tauri dev
npm run build
npm run verify:no-net
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
```

[Installation/configuration](docs/internal-frame-install.md) · [Receiver and synthetic tests](upload-server/README.md) · [Current validation](docs/validation.md)

Windows 10 received a short single-screen remote-desktop test on 2026-10-20. Windows 11, physical multiple displays/mixed DPI, full two-workday collection, production uploads and organizational/privacy compliance require separate acceptance. A successful CI build is not proof of those outcomes.
