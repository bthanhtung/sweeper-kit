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
- It does not clean system-wide caches (only your own `~/Library` and home folder). Administrator rights are used only for the leftovers of an app you uninstall (see [Administrator scope](#administrator-scope)).
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

## Leftover Finder

The **Leftovers** section (⌘L starts a scan) lists data in your `~/Library` that no installed app claims, for example caches of an app you deleted by dragging it to the Trash. Groups are sorted by confidence, then size, and each shows a badge:

| Confidence | Badge | Meaning | Pre-selected |
| --- | --- | --- | --- |
| High | Removed app (green) | The app was uninstalled with Sweeper Kit (recorded in `removed-apps.json`) | Yes |
| Medium | Unclaimed bundle ID (amber) | Identifier-shaped name (`com.vendor.app`) that no installed or running app uses | No |
| Low | Name match only (grey) | Plain folder name in Application Support or Caches that no installed app resembles; it may belong to anything | No |

Nothing else is ever pre-selected. "Select all high confidence" ticks only High groups; Low groups can only be ticked by hand. Removal follows the usual flow: dry run listing exact paths, Trash by default, a report with failures and the number of items left after a rescan.

What is excluded: anything an installed app owns (exact identifier, vendor prefix or child identifier, app name), identifiers of running apps or apps found elsewhere by Spotlight, Apple's own entries (`com.apple.*`, Apple containers) and a list of system and common vendor folders. Items refused by the path policy are shown greyed out.

### Custom orphan rules

`~/Library/Application Support/Sweeper Kit/orphan-rules.json` is merged into the bundled `rules/macos-orphans.json`. A location with the same `id` replaces the bundled one, new ids are added, and exclusions are appended. An invalid file is ignored and a notice explains why.

```json
{
  "version": 1,
  "locations": [
    { "id": "logs", "category": "logs", "base": "~/Library/Logs", "suffixes": [""] }
  ],
  "exclude_names": ["MyCompanyTool"],
  "exclude_prefixes": ["com.mycompany."]
}
```

- `base` must start with `~/Library/` and contain no `..`; `suffixes` are stripped from entry names to get the candidate key (`""` keeps the name as is).
- `category` is one of `app_data` (default), `caches`, `preferences`, `cookies_state`, `logs`, `background_items`.
- `exclude_names` / `exclude_prefixes` are case-insensitive and never match as orphans.
- Plain (non-identifier) names are only considered in the `application_support` and `caches` locations.

## History & Restore

The **History** section lists everything Sweeper Kit moved to the Trash, newest removal first, with date, item count and size. Expand a removal to see each item's original path, size and status: **In Trash** or **No longer in Trash**. Tick restorable items, choose **Restore…**, review what goes back where, and get a per-item result. Restore never overwrites an existing file or folder, and every destination is checked against the path policy. Entries whose Trash item is gone can be dropped with **Remove from history**; entries still in the Trash cannot. The uninstaller report links here ("View in History").

Items removed with administrator access appear with a **System item** badge and the status **In staging folder**. Restoring them needs the administrator password again (one prompt for all of them in the same request); the dialog warns about it first.

Settings → **Removed apps history** lists the apps that count as "removed with Sweeper Kit". **Forget** drops one, which lowers the confidence of its future leftovers from High to Medium.

## Administrator scope

Some things an app leaves behind cannot be removed with your normal rights. Sweeper Kit marks them with a lock badge, **Needs administrator password**, and lists them in a **System** group in the detail view:

- launch daemons and agents in `/Library/LaunchDaemons` and `/Library/LaunchAgents`, and the app's data in the other machine-wide `/Library` folders (Application Support, Caches, Preferences, Logs, Privileged Helper Tools, …);
- an app bundle that is root-owned or sits in a folder you cannot write to.

**One prompt per operation.** The dry-run sheet lists these items separately under "Requires administrator password (one prompt)". When you confirm, the app starts the bundled `sweeper-helper` through `osascript … with administrator privileges` once, and macOS asks for your password once. Launch daemons are unloaded (`launchctl bootout`), the items are moved, and any receipts you chose are forgotten, all in that single run. If you cancel the prompt, the report says "Administrator authorization was cancelled", the items stay where they are and are listed as left over; your own files are still moved to the Trash.

**Staging, not deletion.** The helper never deletes. It moves each item to `/Library/Application Support/Sweeper Kit/Staging/<batch>/`, a root-owned folder, and the move is recorded in the undo log. The helper only accepts paths allowed by a narrow system policy (children of a few `/Library` folders and `*.app` bundles directly in `/Applications`), re-checks every path right before touching it, and refuses anything Apple, protected, or belonging to Sweeper Kit itself. Its actions are logged (action and path only) to `/Library/Logs/Sweeper Kit/helper.log`.

**Restore.** Staged items show up in History with a **System item** badge. Restoring them needs the password again, once for all of them in a request; an existing item at the original path is never overwritten.

**Without the helper** (for example some development runs where `sweeper-helper` is not next to the executable), administrator items are shown unchecked and disabled with an explanation, and restoring them fails with a clear message. The helper is only used if it is a regular file, not a symlink.

Limits:

- `osascript` cannot prove which binary it elevates. The helper must therefore live where only administrators can write, which is the signed app in `/Applications`; do not run an elevated build from a folder other users can write to.
- Items are moved with `rename(2)` only. An item on a different volume than `/Library` is reported as failed instead of being copied as root.
- SIP-protected locations and files protected by TCC can fail even as root; the failure is reported per item.
- Staged items are root-owned and cannot be inspected by the app, so only the helper can say whether one is still there.- A signed `SMAppService` daemon with an XPC interface would avoid the repeated prompts and is the intended future option.

## Installer receipts

Apps installed from a `.pkg` leave a record in the macOS receipt database. In the detail view, **Installer receipts** lists the packages that belong to the app (identifier equal to, or a child of, its bundle identifier, also with the developer's team id in front) with version and install location. Apple receipts are never shown.

Each has a **Forget receipt** checkbox, off by default: "Removes the installer record. Files listed by the receipt are not deleted by this step." The receipts you tick are forgotten with `pkgutil --forget` inside the same administrator run as the other system items. The app re-reads the receipts on its side and rejects any identifier it did not find for that app.

## Safety model

- **Dry run first.** Nothing is removed until you confirm a sheet listing every path and the total size.
- **Trash by default.** Items are moved to the Trash. Permanent deletion is an explicit setting, and the confirm button turns red when it is on.
- **Undo log.** Every Trash move is recorded in `~/Library/Application Support/Sweeper Kit/undo-log.json` (original path, Trash location, size). The History view restores from it (see [History & Restore](#history--restore)). Every action is also appended to `~/Library/Logs/Sweeper Kit/sweeper-kit.log` (paths only, never file contents).
- **Path policy deny list.** Every path is validated against a `PathPolicy` when it is scanned and again immediately before it is removed: it must be absolute, inside an allowed root (your app folders or specific `~/Library` folders), and not an Apple container, system location, Sweeper Kit's own data, a symlink escaping the roots, or anything on your protected list. Refused items are shown greyed out with the reason and cannot be checked.
- **Matching order.** Leftovers are attributed by exact bundle identifier first, then by identifier prefix or containment where a location allows it, then by app name (only for names of three or more characters), and shared team-wide entries are listed but never pre-checked. Files that belong to another installed app are skipped.
- **Minimal privilege.** Only `sweeper-helper` ever runs as root. It takes one argument (a size-limited JSON manifest), validates the whole manifest against a separate, narrower system path policy before changing anything, runs only `launchctl` and `pkgutil` by absolute path with an empty environment, and moves items to a staging folder instead of deleting them. System items are never selected by app name alone.
- **No network.** The webview has a strict Content Security Policy (`default-src 'self'`), the app has no HTTP, shell or updater plugin, and the UI loads nothing from the internet. The webview can only call the app's own commands, native dialogs and event listeners.

## Build

Requires Rust (1.80+) and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`). There is no npm or bundler: the UI in `ui/` is static HTML, CSS and ES modules.

```sh
cargo test --workspace            # core library tests and the app smoke tests
cd crates/sweeper-app
cargo tauri build --bundles app   # -> target/release/bundle/macos/Sweeper Kit.app
```

The bundle also contains the privileged helper at `Sweeper Kit.app/Contents/MacOS/sweeper-helper` (Tauri `externalBin`). `crates/sweeper-app/build.rs` builds `sweeper-helper` for the current target and stages it as `crates/sweeper-app/binaries/sweeper-helper-<target triple>` (git-ignored), so no extra step is needed. For `--target universal-apple-darwin` Tauri wants one fat binary, so build both triples yourself and combine them first (the CI workflow does exactly this):

```sh
for t in aarch64-apple-darwin x86_64-apple-darwin; do cargo build --release -p sweeper-helper --target $t; done
mkdir -p crates/sweeper-app/binaries
lipo -create -output crates/sweeper-app/binaries/sweeper-helper-universal-apple-darwin \
  target/aarch64-apple-darwin/release/sweeper-helper target/x86_64-apple-darwin/release/sweeper-helper
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

**Can I get items back?** Yes, unless you turned on "Delete immediately": open **History**, tick the items and choose Restore (or use Put Back in the Trash).

**Why is an item greyed out?** The path policy refused it (Apple app, protected item, or a location Sweeper Kit never touches). The reason is shown under the item.

**Why is a Group Containers entry unchecked?** It is shared by every app from the same developer, so it is never selected automatically.

**Can I protect something?** Yes, in Settings add a path or bundle identifier to the protected list.

**Why does macOS ask for my password?** Some leftovers (launch daemons, `/Library` data) and apps installed by an administrator cannot be changed with your normal rights. Items marked "Needs administrator password" are removed in one step with a single prompt. Cancelling leaves them in place; everything else is still removed.

**Why is "Forget receipt" off by default?** Forgetting a receipt only removes the installer's record that the package was installed; it does not delete any files. Most people do not need it, so it is opt-in.

**Why are administrator items disabled?** The privileged helper (`Contents/MacOS/sweeper-helper`) was not found next to the app executable, which happens in some development runs. Use a bundle built with `cargo tauri build`.

## Milestones

| Milestone | Status |
| --- | --- |
| M1: Uninstaller (core library, app, UI, smoke tests) | Done |
| M2: Cache & Junk Cleaner (rules, core, UI, smoke tests) | Done |
| M3: Leftover Finder and History & Restore (core, UI, smoke tests) | Done |
| M4: administrator scope, installer receipts, bundled privileged helper | Done |

## License

MIT, see [LICENSE](LICENSE).
