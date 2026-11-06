# Sweeper Kit

A free, open-source (MIT), fully offline app uninstaller for macOS. Pick an app, see every file it left in your Library, review the exact list, and move it all to the Trash.

## What it does

- Lists the apps in `/Applications`, `/Applications/Utilities`, `~/Applications` and any folders you add, with version, bundle identifier, size, last-used date and source (App Store, Homebrew or other).
- Finds the files an app leaves behind: application data, caches, preferences, cookies and saved state, logs, and background items (LaunchAgents).
- Shows a **dry run** (the exact paths and total size) before anything is touched.
- Quits the running app first (polite quit, then an explicit "Force quit?" if it refuses).
- Unloads the app's LaunchAgents, removes the files, clears cached preferences, unregisters the app from Launch Services, and rescans to prove nothing is left.
- For Homebrew casks, shows the exact `brew uninstall --cask --zap <cask>` command and can run it after you confirm.
- Accepts an app dropped on the window or on the Dock icon.

## What it does not do

- It never touches Apple apps (they are read-only) or anything inside the deny list below.
- It has no network code: no telemetry, no update checks, no accounts.
- It does not clean system-wide caches (only your own `~/Library` and home folder), find orphaned leftovers of already-deleted apps, restore from the Trash, or work with administrator rights. Those are planned (see the milestones).
- It does not manage the login items of other apps beyond the System Events attempt described in [Limitations](#limitations).

## Cache & Junk Cleaner

The **Cleaner** section (⌘R starts a scan) lists files that apps and tools regenerate. Nothing is selected for removal except safe caches; you review a dry run before anything happens. Categories:

| Category | What it lists |
| --- | --- |
| App caches | Each folder in `~/Library/Caches` (browser, Homebrew and tool caches have their own entries) |
| Logs | App logs in `~/Library/Logs` and crash reports (Sweeper Kit's own log is excluded) |
| Browser caches | Cache, Code Cache, GPUCache and Service Worker cache folders of Safari, Chrome, Edge, Brave, Arc and Firefox |
| Developer | Xcode DerivedData, Archives and DeviceSupport, simulator caches, npm, Yarn, pnpm, Cargo, pip, Gradle, CocoaPods and Go build caches, Homebrew downloads, plus the actions below |
| Trash | Every item in `~/.Trash` (never selected by default) |
| Old downloads | Items in `~/Downloads` not modified for N days (default 30, change it in Settings; never selected by default) |

**Never touched:** browser history, cookies, saved passwords, bookmarks, autofill and profile preferences; Documents, Desktop, Mail, Keychains and anything else outside the cache locations above. Rules cannot target those names and the path policy refuses them again before every removal.

- **Trash is permanent.** Everything else is moved to the Trash and can be put back. Items you clean from the *Trash* category are deleted for good; the dry run lists them in a red section ("These items cannot be restored") and asks for a second confirmation ("Empty Trash permanently?"). The "Delete immediately" setting does not apply to the Cleaner: its items always go to the Trash.
- **Actions** run a fixed command instead of deleting files, and the sheet shows the exact command line: `xcrun simctl delete unavailable`, `brew cleanup -s` and `docker system prune -f`. Tools are looked up at fixed system locations (never through `PATH`) and an action only appears when its tool is installed. Docker is never selected by default and needs its own confirmation: "Run docker system prune -f? This removes stopped containers, unused networks, dangling images and build cache." Stopped containers and unused networks are lost; volumes are kept.
- **Running-app guard.** If the app a cache belongs to is running (for example Chrome), its entries come back unchecked with a warning, because a running app may be using them. Quit the app, scan again, or tick them deliberately.
- **Minimum item size.** Settings › Cleaner hides entries below a size you choose; actions are always listed.
- **Only ids cross the UI boundary.** The UI sends the ids of the entries you ticked; the app rebuilds the plan from its own scan, rejects unknown ids and ignores refused entries.

### Custom cleaner rules

What the cleaner lists is data: `rules/macos-cleaner.json`. Your own rules go in `~/Library/Application Support/Sweeper Kit/cleaner-rules.json` and are merged over the bundled ones at startup (a category or item with an existing `id` replaces the bundled one, a new `id` is added). An invalid file is ignored and the reason is shown in the Cleaner.

```json
{
  "version": 1,
  "categories": [
    {
      "id": "developer",
      "title": "Developer",
      "items": [
        {
          "id": "bun",
          "title": "Bun cache",
          "paths": ["~/.bun/install/cache"],
          "default_checked": true
        }
      ]
    }
  ]
}
```

- Category: `id`, `title`, `items`. Item: `id` and `title` (no `/`), then either `paths` or `action`.
- `paths`: patterns starting with `~/`, plain segments and `*` inside a segment (no `..`, no `**`). `each_child` lists the children of the folder instead of the folder itself; `exclude_children` names children to skip.
- `app_bundle_id`: if that app is running, the item is unchecked with a warning.
- `default_checked`: selected after a scan. `min_age_days`: only items not modified for that long (the Settings value is used for Downloads).
- `mode`: `trash` (default) or `delete_only`, which is only accepted for paths inside `~/.Trash`.
- Rules whose last path segment is personal browser data (History, Cookies, Login Data, Web Data, Bookmarks, Favicons, `places.sqlite`, `key4.db`, `logins.json`, Preferences, Local State, ...) are rejected, and locations must lie inside your home folder and outside protected areas.
- `action` is chosen **by id only**: `simctl_delete_unavailable`, `brew_cleanup` or `docker_prune`. Rules cannot supply commands or arguments.

## Safety model

- **Dry run first.** Nothing is removed until you confirm a sheet listing every path and the total size.
- **Trash by default.** Items are moved to the Trash. Permanent deletion is an explicit setting, and the confirm button turns red when it is on.
- **Undo log.** Every Trash move is recorded in `~/Library/Application Support/Sweeper Kit/undo-log.json` (original path, Trash location, size). Restoring from the UI arrives in a later milestone; the data is already there. Every action is also appended to `~/Library/Logs/Sweeper Kit/sweeper-kit.log` (paths only, never file contents).
- **Path policy deny list.** Every path is validated against a `PathPolicy` when it is scanned and again immediately before it is removed: it must be absolute, inside an allowed root (your app folders or specific `~/Library` folders), and not an Apple container, system location, Sweeper Kit's own data, a symlink escaping the roots, or anything on your protected list. Refused items are shown greyed out with the reason and cannot be checked.
- **Matching order.** Leftovers are attributed by exact bundle identifier first, then by identifier prefix or containment where a location allows it, then by app name (only for names of three or more characters), and shared team-wide entries are listed but never pre-checked. Files that belong to another installed app are skipped.
- **No network.** The webview has a strict Content Security Policy (`default-src 'self'`), the app has no HTTP, shell or updater plugin, and the UI loads nothing from the internet. The webview can only call the app's own commands, native dialogs and event listeners.

## Build

Requires Rust (1.80+) and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`). There is no npm or bundler: the UI in `ui/` is static HTML, CSS and ES modules.

```sh
cargo test --workspace            # core library tests and the app smoke tests
cd crates/sweeper-app
cargo tauri build --bundles app   # -> target/release/bundle/macos/Sweeper Kit.app
```

The icon set is generated, not hand-drawn: `python3 crates/sweeper-app/icons/make_icon.py` writes `icons/source.png` (standard library only), then `cargo tauri icon icons/source.png` inside `crates/sweeper-app` produces the rest.

## Full Disk Access

macOS protects some locations (for example parts of `~/Library/Containers` and `~/Library/Mail`). Without Full Disk Access Sweeper Kit lists what it could not read in a notice and shows a banner when it detects the permission is missing. To grant it: **System Settings › Privacy & Security › Full Disk Access**, turn on Sweeper Kit, then reopen the app. The banner's "Open System Settings" button jumps straight there.

## First run of an unsigned build

Builds you compile yourself are not signed or notarized, so Gatekeeper may refuse to open them. After copying the app to `/Applications`, clear the quarantine flag:

```sh
xattr -cr "/Applications/Sweeper Kit.app"
```

## Custom rules

Where leftovers live is data, not code: `rules/macos.json`. Each entry is a location:

```json
{
  "id": "caches",
  "category": "caches",
  "base": "~/Library/Caches",
  "match": "id_or_name",
  "suffixes": [""],
  "default_checked": true
}
```

- `id`: unique name. A user rule with an existing `id` replaces the bundled one; a new `id` is added.
- `category`: `app_data`, `caches`, `preferences`, `cookies_state`, `logs` or `background_items`.
- `base`: the folder to list; must start with `~/Library/`.
- `match`: `id`, `id_prefix`, `name`, `id_or_name`, `team_or_id`, `name_prefix` or `contains_id`.
- `suffixes`: appended to the identifier or name when matching (for example `[".plist"]`).
- `default_checked`: whether the preset "Everything" checks it.

Your own rules go in `~/Library/Application Support/Sweeper Kit/rules.json`, in the same format (`{"version": 1, "locations": [...]}`). They are merged over the bundled rules at startup; an invalid file is ignored and the reason is shown in Settings. The path policy still applies to everything your rules find.

## Limitations

- Login items of other apps can only be removed through System Events (AppleScript); macOS may ask for Automation permission, and a failure is reported as a warning rather than an error.
- There is no automated UI test: XCUITest needs Xcode and Tauri's WebDriver does not support macOS. Instead `crates/sweeper-app/tests/smoke.rs` drives the full command layer (list, scan, dry run, quit, uninstall, undo log, settings) against a dummy app in a fake home with test doubles; `cargo test --workspace` runs it.
- An app opened from outside the app folders (for example `~/Downloads`) can be scanned, but its bundle is refused by the path policy; its leftovers can still be removed.
- Windows and Linux are planned; today only macOS 14 or later is supported.

## FAQ

**Is anything sent anywhere?** No. There is no networking code and the webview is locked down by its CSP.

**Can I get items back?** Yes, unless you turned on "Delete immediately": open the Trash and use Put Back. The undo log keeps the original locations.

**Why is an item greyed out?** The path policy refused it (Apple app, protected item, or a location Sweeper Kit never touches). The reason is shown under the item.

**Why is a Group Containers entry unchecked?** It is shared by every app from the same developer, so it is never selected automatically.

**Can I protect something?** Yes, in Settings add a path or bundle identifier to the protected list.

## Milestones

| Milestone | Status |
| --- | --- |
| M1: Uninstaller (core library, app, UI, smoke tests) | Done |
| M2: Cache & Junk Cleaner (rules, core, UI, smoke tests) | Done |
| M3–M4: leftovers of already-deleted apps, restore UI for the undo log | Planned |

## License

MIT, see [LICENSE](LICENSE).
