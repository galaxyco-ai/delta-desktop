# Delta desktop (Windows)

Thin [Tauri v2](https://v2.tauri.app/) shell around the Delta web app at
**https://delta.galaxyco.ai**. It does not bundle the web app. Release builds
open the sign-in page at `https://delta.galaxyco.ai/sign-in` and dev builds open
`http://localhost:3000/sign-in`. Users who already have a session are sent on to
the app by Clerk.

## What works in V1

| Feature | Behaviour |
| --- | --- |
| Window | 1280x800, minimum 900x600. Position and size are restored by `tauri-plugin-window-state`. Closing hides to the tray instead of quitting. |
| Start page | Release builds open `https://delta.galaxyco.ai/sign-in`. `npm run dev` opens `http://localhost:3000/sign-in`. Set by `app.windows[0].url` (`"sign-in"`) in `tauri.conf.json`, resolved against `frontendDist` / `devUrl`. A signed-in user is redirected to the workspace. |
| System tray | Left-click shows the window. Menu: Show, Hide, Sign in in browser, Test notification, Check for updates, Start with Windows (toggle), Quit. |
| Notifications | Native Windows toasts via the notification plugin. The loaded site can call them (see [Notification bridge](#notification-bridge)). Capability is scoped to `delta.galaxyco.ai` and `localhost:3000`. |
| Deep links | Registers `delta://`. `delta://auth?ticket=...` focuses the window and navigates to `/desktop/auth/consume?ticket=...` on the current origin. The matching web routes are not built yet (see [Sign-in](#sign-in)). |
| Auto-update | Fully automatic and silent. See [Auto-update policy](#auto-update-policy). |
| Start with Windows | On by default (per-user `Run` registry entry via `tauri-plugin-autostart`). Login launches start hidden in the tray. Toggle from the tray menu; turning it off sticks. |
| Installer | NSIS and MSI, **unsigned** (no Authenticode certificate yet). |

## Run locally

Needs Windows with WebView2 (current Windows 10/11 already have it), Rust 1.90+,
and Node.

```bash
cd desktop
npm install
npm run dev     # tauri dev, opens http://localhost:3000/sign-in (run the web app there first)
npm run build   # release build, unsigned
```

Artifacts land under `desktop/src-tauri/target/release/bundle/{nsis,msi}/`.

## Building on Linux (cross-compile)

A Linux host can produce the Windows NSIS installer with
[cargo-xwin](https://github.com/rust-cross/cargo-xwin) and the
`x86_64-pc-windows-msvc` target. MSI is not supported on this path.

Install Rust 1.90+, `cargo-xwin`, `nsis`, `clang`/`lld`/`llvm`, `pkg-config`, and
`libayatana-appindicator3-dev` (the Tauri CLI probes that library on Linux
hosts because of the tray icon, even when the target is Windows). On Debian,
`clang-cl` may need a symlink to `clang-cl-19`.

```bash
cd desktop
npm install
npm run tauri build -- --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
```

Set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the
environment to get updater `.sig` files.

## Notification bridge

`withGlobalTauri` is on, and the default capability allows the notification
plugin for the two origins above. From any page on those origins:

```js
const api = window.__TAURI__?.notification;
if (api) {
  const granted = (await api.isPermissionGranted())
    || (await api.requestPermission()) === "granted";
  if (granted) {
    api.sendNotification({ title: "Delta", body: "A ticket needs you." });
  }
}
```

The tray item "Test notification" exercises the native path.

## Sign-in

Some identity providers (Google included) reject embedded WebView2 sign-in.
The shell therefore offers **Sign in in browser**, which opens
`{origin}/sign-in` in the system browser, and listens for
`delta://auth?ticket=<token>` to hand a short-lived sign-in token back to the
webview. The web-side routes that mint and consume that token are a planned
follow-up in the web app. Until they exist, email-based sign-in inside the
window is the reliable path.

## Auto-update policy

Nobody has to click anything. Implemented in `src-tauri/src/lib.rs`.

1. **At launch:** about 5 seconds after start, Delta checks the endpoint. If a
   newer version exists it downloads and installs immediately, since the app
   has only just opened.
2. **While running:** it checks again every 4 hours. A found update downloads
   in the background and waits for a quiet moment before installing: the
   window is hidden to the tray, minimized, or has not had focus for 15
   minutes (re-checked every minute).
3. **Install:** the NSIS installer runs in quiet mode
   (`plugins.updater.windows.installMode = "quiet"`, so `/S /UPDATE /R`). The
   app exits, the per-user installer replaces files with no window and no
   admin prompt, closes any leftover instance without asking, and relaunches
   Delta with the same arguments. If the window was hidden or minimized at
   install time, the relaunched app stays in the tray.
4. **After:** the first launch of a new version shows one toast, "Delta
   updated to vX". No other update UI exists.
5. Tray **Check for updates** installs right away if an update is available,
   otherwise it says you are on the latest version.

Debug builds (`npm run dev`) never auto-update and never register autostart.

### Signing keys

The **public** key lives in `src-tauri/tauri.conf.json` at
`plugins.updater.pubkey`. The private key and its password are repo Actions
secrets (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) and
are never committed. Updates whose signature does not verify are rejected.

Update endpoint:

`https://github.com/galaxyco-ai/delta-desktop/releases/latest/download/latest.json`

## Cut a release

1. Bump `version` to the same semver in `desktop/package.json`,
   `desktop/src-tauri/tauri.conf.json`, and `desktop/src-tauri/Cargo.toml`.
2. Tag and push:
   ```bash
   git tag desktop-v0.1.3
   git push origin desktop-v0.1.3
   ```
3. [`.github/workflows/desktop.yml`](../.github/workflows/desktop.yml) builds
   NSIS and MSI on `windows-latest` and publishes a GitHub Release with the
   installers, `.sig` files, and `latest.json`.
4. Running the workflow by hand (Actions, Desktop, Run workflow) validates a
   build and uploads the installers as a workflow artifact without publishing
   a release.

## Code signing

Builds are not Authenticode signed, so Windows SmartScreen shows "Windows
protected your PC". Click **More info**, then **Run anyway**.

Options for removing the warning later:

| Option | Notes |
| --- | --- |
| [Azure Trusted Signing](https://learn.microsoft.com/en-us/azure/trusted-signing/) | Cheapest Microsoft-trusted path, roughly $10/month. Works with GitHub Actions. |
| [SignPath](https://signpath.io/) | Free tier is for open-source projects only (this repo is `UNLICENSED`). |
| OV / EV certificate from a public CA | Usually more expensive than Azure Trusted Signing. |

## Layout

```
desktop/
  package.json   npm scripts: dev, build
  README.md      this file
  src-tauri/     Rust host: window, tray, deep links, notifications, updater, autostart
```
