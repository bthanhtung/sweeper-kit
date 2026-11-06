# Contributing to Sweeper Kit

Thanks for helping. Sweeper Kit deletes files, so the bar for safety is higher than for most projects. Read this before you change code.

## Goals

- **Offline.** No network code, no telemetry, no update checks, no accounts.
- **Safe.** Dry run before anything changes, Trash by default, and every path checked by one gate.
- **Simple.** Data (rules) lives in JSON; code stays small and testable.

## Development setup

- Rust stable (1.80 or newer) via [rustup](https://rustup.rs).
- Tauri CLI: `cargo install tauri-cli --version "^2" --locked`
- macOS 14 or newer. Xcode is not needed.
- There is no npm or bundler. The UI in `ui/` is static HTML, CSS and ES modules.

## Layout

| Path | What it is |
| --- | --- |
| `crates/sweeper-core` | Library: scanning, matching, path policy, removal, cleaner rules, test doubles (`testing` module) |
| `crates/sweeper-app` | Tauri 2 app: commands, native integration, smoke tests in `tests/` |
| `ui/` | Static front end (no build step) |
| `rules/` | Bundled data: `macos.json` (leftover locations) and `macos-cleaner.json` (cache and junk rules) |

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cd crates/sweeper-app && cargo tauri build --bundles app
```

CI runs the first three checks and builds the DMG (see `.github/workflows/build.yml`). Run them locally before you open a PR.

## Safety rules

Every change must keep these rules:

- **Dry run first.** Nothing is removed without a confirmed plan that lists every path and the total size.
- **Trash by default.** Permanent deletion only where the product already allows it (Trash category, explicit setting).
- **One gate.** Every path goes through `PathPolicy` when it is scanned and again right before removal. Do not add a second check or skip the gate.
- **Exact matching.** Attribute leftovers by exact bundle identifier, then team, then exact name. Never match by substring.
- **No network.** Do not add HTTP, shell or updater plugins, and do not relax the CSP (`default-src 'self'`).
- **Arguments as arrays.** Run external tools with an argument array, never a shell string. Look tools up at fixed system paths, not through `PATH`.
- **Tests use a fake home.** Build fixtures with `sweeper_core::testing`. Tests must never touch the real home directory, Trash or system tools.
- **Only ids cross the UI boundary.** The UI sends ids; the app rebuilds the plan from its own scan.

## Adding rules

- Leftover locations: edit `rules/macos.json`. Each entry needs a unique `id`, a `category`, a `base` under `~/Library/`, and a `match` mode. The format is described in the "Custom rules" section of the README.
- Cache and junk rules: edit `rules/macos-cleaner.json`. The format is described in the "Custom cleaner rules" section of the README.
- Rules cannot name personal browser data or locations outside your home folder. Keep it that way, and add a test for any new rule shape.

## Commit style

- [Conventional Commits](https://www.conventionalcommits.org/): `feat(core): ...`, `fix(app): ...`, `docs: ...`, `test: ...`.
- Keep commits small and scoped to one module.
- Write the subject in the imperative mood, under about 72 characters.

## Pull request checklist

- [ ] `cargo fmt`, `cargo clippy -D warnings` and `cargo test --workspace` pass locally.
- [ ] Safety rules above still hold. Path checks and dry-run flow are unchanged, or the change is explained in the PR.
- [ ] New behaviour has a test that runs on a fake home.
- [ ] README and rules docs are updated when behaviour or rule formats change.
- [ ] No new dependencies that add network access or change the lockfile without a reason.

## License

Sweeper Kit is released under the MIT License (see [LICENSE](LICENSE)). By contributing, you agree that your contributions are licensed under the same terms.
