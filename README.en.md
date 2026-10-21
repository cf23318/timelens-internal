# TimeLens Internal

An internal desktop adaptation of [gitxuzhefeng/timelines](https://github.com/gitxuzhefeng/timelines) v0.17.5 (f8e7107), under the [MIT license](LICENSE). Current source test version: **0.17.10**; check Releases for published installers. See the [Chinese README](README.md) for full configuration and acceptance details.

- Chinese and light appearance by default; collection starts manually. Pause/resume is available, and restarting leaves collection paused.
- Periodic WebP screenshots (12-second default attempt interval), foreground-window events, local timeline and replay. Replay works with OCR disabled; optional OCR runs locally. This is sampled screenshots, not continuous video.
- New tasks default to 48 accumulated local collection hours. The foreground display is captured by default; all displays are optional. Windows input counters are not implemented.
- Foreground privacy rules block listed apps, sensitive titles and banking domains. Unreadable browser addresses block screenshots. Background content may still appear; there is no pixel masking. New titles are redacted; old records are not rewritten automatically.
- Upload is off until configured and explicitly confirmed. New profiles use direct OSS uploads with imported grants valid for up to seven days; the administrator UI only exposes OSS. Shared grants group uploads by device but do not isolate cloud write permissions between devices in the batch. Each original WebP and required metadata is uploaded; the activity database stays local. Legacy receiver tasks remain supported. No new age archive is created. Explicitly enrolled tasks expire within 30 days after the earlier known actual/approved end; legacy profiles are not adopted. Restarting disables image upload again. Direct OSS reconciliation, readback and expiry deletion require the dedicated administrator tools and an online management machine.
- Version 0.17.10 unifies task and toolbar receipt status and retries connection failures and HTTP 408/429/5xx with persisted backoff. OSS acceptance remains distinct from administrator reconciliation; ambiguous 409 responses are never treated as successful uploads. Capture quality and collection rules are unchanged.
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

Windows 10 received a short single-screen remote-desktop test on 2026-10-20. Windows 11, physical multiple displays/mixed DPI, full 48-hour collection, production uploads and organizational/privacy compliance require separate acceptance. A successful CI build is not proof of those outcomes.

## Local archive/retention revision

New task enrollment uses the v2 receiver protocol. Receipts, OSS listings and paginated manifests are reconciled; original readback checks bytes, SHA256 and CRC64. Explicit upload confirmation binds deletion no later than 30 days after the earlier actual/approved study end, without extension on retry. Local original/derived records and managed server restores share the deadline. Legacy profiles are never auto-adopted. This does not introduce 48-hour server accounting, mandatory displays or schedules. See [the archive guide](docs/archive-retention.md).

## Offline device grants and direct OSS upload

The local adaptation also supports a persistent per-profile device ID, a Mac offline grant generator and device-scoped POST V4 uploads to Beijing OSS. See [the offline-grant guide](docs/oss-offline-upload.md). Importing does not start capture or upload. The administrator Secret stays off employee clients.

“OSS accepted” means POST response and CRC validation, distinct from receiver inventory reconciliation/readback. Direct objects use a separate administrator registry and retention worker. Local task retention still applies; keep the dedicated administrator process online for cloud and managed-copy deletion.

Direct OSS administration (0.17.7): new grants include paged inventory uploads. The `timelens-oss-admin` tool reconciles expected objects, verifies originals, restores to managed private storage, and deletes expired task scopes. Use the same private registry for every grant and renewal. A continuously running, online Mac/Linux administrator process is required; an offline or sleeping Mac cannot enforce cloud deletion. See [offline grant operations](docs/oss-offline-upload.md). Internal unsigned packages are not notarized production releases.
