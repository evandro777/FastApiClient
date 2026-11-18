# AGENTS.md: FastApiClient

Notes for AI agents and contributors. Read this before changing anything.

## What this is

A **light and fast** REST API client for Linux, in Rust + Slint: projects with nested folders, environments and `{{variables}}`, `$response` chaining, Insomnia v5 import, a response panel. Priorities: fast start, low memory, simplicity.

## Layout

```txt
Cargo.toml                workspace, release/fast profiles
crates/core/              UI-independent logic (settings.rs). No Slint here, so a CLI can reuse it
crates/app/               Slint UI + main.rs; build.rs compiles ui/ and bundles translations/
crates/app/src/instance.rs  one window: a second start sends `show` over `$XDG_RUNTIME_DIR/<crate>.sock` and exits; the running one is raised. A verbatim copy of FastHello's (change it there, not here)
packaging/                .desktop, SVG icon, 256px PNG icon (the AppImage .DirIcon), metainfo (AppImage, Flatpak and CI validation)
appimage/build.sh         AppImage + .zsync (host libs, not bundled); CI: appimage.yml
flatpak/                  manifest, build.sh (vendors crates, builds outside the repo: `~/.cache/fastapiclient-flatpak`), new-signing-key.sh, signing-key.b64 (public half, after you run it); CI: flatpak.yml
docs/                     landing page (`site/index.html`, deployed with the Flatpak remote), `screenshots/`, FLATPAK-SIGNING.md, FLATHUB.md
README.md, BUILD.md       what users read / build, settings and details (keep both in step with behaviour)
dev/gen_po.py             writes the .po files from one table
scripts/ui-snapshot.sh    renders pages on a private Xvfb with canned data (crates/app/src/fixtures.rs, env FASTAPICLIENT_FIXTURES=empty|single|many, FASTAPICLIENT_PAGE=<n>) to target/ui-snapshots/
```

## Commands

```bash
cargo build --profile fast    # iteration build, no LTO
cargo build --release         # thin LTO
cargo test                    # unit tests
```

System deps: `libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-dev libxcb1-dev`. No GL/Skia/femtovg.

## Code conventions: Editing code

- Use the Edit tool for code changes. Don't splice files with sed, awk, python or heredocs through Bash.
- After editing Rust, run `cargo fmt && cargo clippy -- -D warnings && cargo test`. Re-read the surrounding block to confirm no braces, lines or whole components (e.g. pages) were deleted.
- After changing dependencies (`Cargo.toml` or `Cargo.lock`), also run `cargo audit`: CI runs it and fails on known vulnerabilities (warnings such as "unmaintained" do not fail). Fix with `cargo update -p <crate>`.
- After any change, also run `cargo build --profile fast` so the binary is built and cached for the owner's manual testing.

## Architecture decisions (inherited from FastMoji, measured there)

- **Software renderer only** (`winit` + `software`, forced in `main.rs` and in `Cargo.toml` features with `default-features = false`): ~27 MB RSS versus ~108 MB with the GL renderer.
- No async runtime. Scans/work in threads, results handed to the UI thread with `slint::invoke_from_event_loop`.
- Never hold a `RefCell` borrow while calling something that makes Slint re-read a model.
- Keep dependencies few and light.
- Slint notes: a `VerticalLayout` inside a `ScrollView` needs `alignment: start`, or a short list spreads its rows over the window; read-only `TextInput` (one per block) makes text selectable; use `content-y` (not `viewport-y`); a callback named `close` shadows a builtin (use `dismiss`); `FocusScope.capture-key-pressed` intercepts keys before `LineEdit`; a wrapped read-only `TextInput` in a nested layout reports one line of height, so give it room with `min-height`, estimated from the available width (a fixed character count leaves a blank band under lines that still fit on one line); `Text` and `TextInput` have no `padding`, so put the space in the parent layout's `spacing` or `padding`.
- Pane positions are preferences (`sidebar_width`, `request_split`), saved on close. Clamp only displayed sizes when the window shrinks. The app-local `divider.slint` uses `absolute-position + mouse-x/y` so moving the handle does not change the drag origin; TouchArea keeps the pointer until release. `dev/check-panes.py` checks both dividers and isolated restart persistence on private Xvfb. Snapshot pages `response-normal`, `response-find`, `response-notice` cover the compact header; `OUT` keeps separate screenshot matrices.

## Dialogs

- `TextInput.set-selection-offsets(anchor, caret)` uses UTF-8 byte offsets, with the anchor first. Assigning editor text from code bypasses native undo; `CodeEdit` keeps the previous text and selection for indentation undo. `dev/check-payload-editor.py` verifies keyboard behavior on private Xvfb.
- The software renderer treats `font-family: "monospace"` as a literal family name. `CodeEditing.monospace-family` resolves the generic family through Slint's shared Fontique collection; use it for both request text and gutter. All paste paths preserve text. JSON/XML use an explicit Format button (`payload.rs`, `CodeEdit.format-body`); a no-op must retain the previous undo entry.

- Request manual formatting/coloring and automatic response beautification use `http::AUTO_TEXT_LIMIT` and `small_text` (64 KiB of UTF-8, inclusive); formatted output must fit too. Raw data responses never format, decode or color text. Manual Beautified processes the captured response (1 MiB capture, 4 MiB output guard), using explicit formatter/highlighter limits. `highlight::response` resets the current cache and applies `auto_beautify`; `response-view(true)` reapplies the preference with automatic limits, `false` applies the toolbar choice. `highlight.rs` formats and colors responses together on the existing lazy worker, caches completed text/colors, and rejects stale results on Raw, request changes and new responses. Response `CodeEdit` consumes those colors without scheduling its own highlighting; request debounce and provisional colors remain independent. `CodeEdit` draws colors behind the native input, retaining native selection and displaying native text during composition (`preedit-text`). On Linux a transparent input also hides its caret, so the colored editor draws a separate caret. Keep the font and line metrics identical in both layers.

- Clear responses through `highlight::clear_response` when changing requests or starting Send. It invalidates worker results synchronously; a Slint `changed has-response` handler runs too late to reject an already queued publication. `response_tests.rs` holds publications to verify these races without X11.

- Slint 1.18's software renderer casts selection rectangles to i16 before clipping: selecting an unwrapped line wider than 32,767 physical pixels panics. `CodeEditing.selection-safe` disables the native background for those lines and uses accent-colored selected glyphs; source text, byte offsets and clipboard operations stay native. Remove this workaround when the renderer clips before casting.

- While waiting for request highlighting, `highlight::edited_colors` splices the current text into the previous token colors at UTF-8 boundaries. Keep its `ColoredText.source` synchronized with the native input: merely displaying the old colored text after an edit hides or misaligns the characters just typed. Background parsing corrects provisional colors after the debounce.

- Syntect's bundled JSON grammar marks keys with `meta.structure.dictionary.key.json`; match its nested `string` scope to override generic string coloring. Token roles are shared by both editors; the VS Code Dark+ / Light+ palette in `code-edit.slint` reacts to appearance changes without reparsing.

- Dialogs (About) are in-window overlays (`component About` as the last child of `Main`), not second windows: closing Main always ends the app and `scripts/ui-snapshot.sh` can capture them (`FASTAPICLIENT_PAGE=about`). Its version comes from `CARGO_PKG_VERSION`, the Website button opens `CARGO_PKG_REPOSITORY` with `xdg-open`.

- A file body (`Body::File`) is never imported (Insomnia or our own export, `without_file_bodies` in `exchange.rs`): a shared collection must not point Send at a local file. The Send worker confirms the resolved path and host before every upload, including dependencies, through `sending::Dialogs` (Prompt kind 8). Snapshot page: `sendfile`.
- `$prompt` uses Prompt kind 9, the same reply-channel bridge, and `chain::prepare_interactive`'s per-Send answer map keyed by the parsed tuple. Never evaluate prompts outside Send or recursively expand answers. Selection/window changes bump generation and cancel pending replies; `Error::Cancelled` stays out of failure messages, including dependency errors. No application/model borrow may cross a dialog wait. Snapshot page: `prompt`.
- Request filtering uses `Workspace::filtered_tree` in both refresh and keyboard navigation. Refresh with `reload: false` preserves edits and selection; clear the transient filter on project changes. Snapshot pages: `filter`, `filter-none`.
- Request/folder creation uses Prompt kinds 10/11 via `ask-create`, shared by sidebar buttons and folder menus. Call Rust creation callbacks only after confirmation; trim/reject blank names before `flush` so dismissal cannot create an item or disturb edits. Rename/F2 uses kind 0 independently. Snapshot pages: `new-folder`, `new-request`, `folder-menu`; `dev/check-creation.py` checks real interactions with temporary projects on private Xvfb.
- Keep Prompt, EnvEditor and Transfer backdrop TouchAreas inside their FocusScopes; a sibling scope intercepts outside clicks when an input has focus. `dev/check-dialogs.py` checks the focused environment editor and both transfer modes on private Xvfb.
- MenuCard (Preferences/Manage) and About must keep both the card and backdrop inside their Escape FocusScope so key events bubble from controls. The Appearance popup consumes the first Escape; the next closes Preferences. `dev/check-dialogs.py` covers this and Tab → Escape in About/Manage.
- Let row popups close automatically: explicit closure can pass the release to the new prompt's backdrop. Prompt's input refocuses once after init because popup closure restores the old focus.

## Translations

- UI strings use `@tr()`; strings built in Rust go through the `Tr` global in `ui/main.slint`. Never `format!` user-visible text in Rust.
- `.po` files are generated by `dev/gen_po.py` (edit its table, run it, commit both). Slint's gettext context is the component name (`Main`, `Tr`). A new `@tr` string needs an entry in every language. A string with a count needs its own singular string for `n == 1` ("1 connection", not "1 connections"), with its own entry.
- Language = `settings.language` (`""` = system locale).
- Appearance = `settings.theme` (`""` = follow the system, `"light"`, `"dark"`; saved as soon as it changes, like `language`; window size and selection are saved when the window closes). The Appearance box sets `Palette.color-scheme` in the `.slint` (`unknown` hands it back to the desktop). `THEMES` and `theme_index` in `crates/app/src/main.rs` keep the order of that list, and a test checks they agree. `Family.dark` reads the drawn background, so it stays right in every state.

## Conventions

- Rust 2024, `rustfmt` max_width 120, clippy `-D warnings`. Comments explain *why*, not what.
- Settings are serde TOML with `#[serde(default)]`; add a unit test for each new option so old files keep loading.
- `files::read_or_backup` returns an error if preserving the original fails; startup must stop before saving defaults. Import opens with Linux `O_NONBLOCK` before checking for a regular file, then bounds the actual read, so a named pipe cannot block during open.
- Settings path: `$XDG_DATA_HOME/FastApiClient/settings.toml`.
- Update `README.md` when behaviour changes, and this file when you learn something non-obvious.

## Testing

- Project writes use `save(ui, app, index)` and a dirty-ID set; `flush` retries failed writes even when editor values are unchanged. Keep failed projects pending across selection changes and block normal close on outstanding failures. `app_tests::save_recovery` injects failures without touching user files. Snapshot: `save-error`.
- `Responses` compares a saved project snapshot before lookup, so environment/definition changes and late results cannot cross contexts. Non-JSON, unsuccessful or truncated refreshes remove old values. Do not bypass this through direct cache insertion.
- Keep serde_json's `arbitrary_precision` feature enabled in core: environments, project files and cached response IDs must preserve large integers and decimal digits. Do not convert numeric substitutions through `f64`.
- Normalize environment names through `Project::normalize_environments` in every external reader, preserving the active entry before changing names. Empty `active_env` always means no values. Header names resolve before inheritance chooses winners; only winning values may evaluate prompts or register response dependencies, and resolved names must never expand recursively.
- Give every app Send a `Responses::for_send` handle using its captured generation; pass that handle through dependencies and the final response store. Its generation checks share the cache mutation lock, so cancelled workers cannot erase or replace newer cached values for the same unchanged project.
- Use `Responses::invalidate` before manual HTTP refreshes too: a network error supplies no response to `store`. Resolve header names before any case-insensitive merge, since template variable names are case-sensitive. File bodies, like imports, must open with `O_NONBLOCK` and reject nonregular files before reading.
- Environment changes call `reset_send` without reloading request editor values; this rejects late HTTP/highlight publications and cancels pending prompts. No-op environment selections/value saves keep the generation. Insomnia GLOBAL_HEADERS convert template keys while preserving variable-name case; raw template keys must not merge case-insensitively.
- Keep ureq redirects disabled: its Authorization policy leaves custom API-key headers exposed. `http::redirect_target` restricts manual redirects to the original origin, including effective port; unsafe redirects and body-preserving 307/308 stay visible. Keep the total timeout and redirect count bounded. Header-name dependencies must resolve before values/auth/body; `chain::Preparation` shares freshness across stages and dependencies, so zero-expiry responses are fetched once per Send while newly discovered expired responses still refresh.
- Drop automatically replaced Content-Length/multipart Content-Type before evaluating values. Response cache data stays as validated JSON bytes with serialized project snapshots, bounded globally to 32 MiB and 256 responses; parse on lookup outside the cache lock. Eviction of a freshly fetched dependency must return `Error::CacheLimit`, never refetch indefinitely. Cancel Sends before `Responses::remove_project` on successful deletion. Native imports reject missing/null payloads but retain defaults for an explicit empty project. Snapshot: `cache-limit`.
- Insomnia compares effective `GLOBAL_HEADERS` across environments before promoting them; reject differences instead of sharing one environment's credentials. Shared `{{token}}` templates are supported. Retain original media types unless explicitly overridden; multipart boundaries are generated. Snapshot: `import-headers`.
- Validation uses the shared 64 KiB limit before parsing. Response XML preserves leaf whitespace and skips mixed content, CDATA and `xml:space="preserve"`. Snapshots: `validation-limit`, `response-xml`.

- `cargo test`. Manual harnesses (if any) go in `dev/`, must never capture the whole screen, and must never click or type at hard-coded coordinates.
- Never `pkill -f` with a pattern that appears in your own command line; kill by PID.

### Verifying UI changes

- Slint `visible: false` still takes layout space. Instantiate body content conditionally so a hidden `CodeEdit` cannot shrink form tables.

- After any Slint UI change, run `scripts/ui-snapshot.sh <page>` and read the PNGs (empty, single, many) before saying the change is done. It uses a private Xvfb and canned data, so it never reads or changes the real system.
- Specifically check: rows don't stretch when a list has 1 item, badges and buttons are right-aligned, icons are sharp, items with no icon get a placeholder so headers stay aligned, vertical gaps between rows of one kind are even, counts read right at 1 ("1 items"), a name already in a header is not repeated in every row under it, text is selectable as a block, and empty or error states (e.g. "nothing found") don't throw errors.
- Controls in a row are the same height (32 px); tabs, search boxes and dialog cards use the kit's components (`TabBar`, `SearchBox`, the card in FastHello's `docs/UI-STYLE.md`).
- If you can't launch it (e.g. the flow needs pkexec or a live X11 session), say so explicitly and list what I should test manually.

## GitHub flow and releases

Work on branches, PRs to `master`. CI: `ci.yml` (fmt, clippy, tests, MSRV, metainfo/desktop validation), `appimage.yml` (AppImage on every push/PR; on a `v*` tag it attaches to the release), `flatpak.yml` (bundle on every push/PR; on a `v*` tag it also publishes the signed remote and the landing page to GitHub Pages, then attaches the bundle), `virustotal.yml` (scans the AppImage and the Flatpak after their workflows; needs the `VIRUSTOTAL_API_KEY` secret). `RELEASING.md` has the one-time setup and the steps. Never commit or push on the owner's behalf.

Release gotchas (each one broke a first release):

- Bump the version in `Cargo.toml`, then run `cargo check` and commit `Cargo.lock`: CI uses `--locked`, so a stale lock fails every job.
- The metainfo needs a `<release version="X">` for the tag, or `flatpak.yml` fails.
- `appimagetool` builds `.DirIcon` from a PNG at the AppDir root, not from the SVG. Regenerate `packaging/io.github.evandro777.FastApiClient.png` when the SVG changes: `convert -background none -density 384 packaging/io.github.evandro777.FastApiClient.svg -resize 256x256 packaging/io.github.evandro777.FastApiClient.png`.
- The first tag needs the one-time setup in `RELEASING.md`: the signing key and its secret, Pages, and the `v*` tag rule on the `github-pages` environment.

## Flatpak

App id `io.github.evandro777.FastApiClient`; `flatpak/build.sh --devel` installs `...FastApiClient.Devel` next to a release. Needs `flatpak-builder` and `org.freedesktop.Sdk//25.08` + `org.freedesktop.Sdk.Extension.rust-stable//25.08`. The runtime version `25.08` appears in five places (the manifest, the container image tag, the rust-stable install and its `GITHUB_PATH` line in `flatpak.yml`, and the header of `flatpak/build.sh`): bump them together.

- Auto-update = our own signed OSTree remote on GitHub Pages (`flatpak.yml`: `bundle` → `pages` → `release`), deployed on `v*` tags only. Every deploy replaces the whole site, so never deploy Pages from another trigger. The public key is `flatpak/signing-key.b64` (base64 of the binary export, made by `flatpak/new-signing-key.sh`; the template ships none); the private half is the secret `FLATPAK_GPG_PRIVATE_KEY`. See `docs/FLATPAK-SIGNING.md`.
- Keep `finish-args` minimal (the manifest says how for X11-only apps and trays). The build vendors crates (`cargo vendor`), so `flatpak/vendor` must not be in the manifest's `skip:`.
- Patterns to copy when an app needs them (FastClipboard has each one working): inside the sandbox `FLATPAK_ID` is set. A single-instance Unix socket must live in `$XDG_RUNTIME_DIR/app/$FLATPAK_ID/`, the only directory shared between `flatpak run`s. A ksni tray needs `.disable_dbus_name(true)` there. Start at login must go through the Background portal (`org.freedesktop.portal.Background.RequestBackground`, blocking, so from a thread), not a file in `~/.config`. A host command (for example `tesseract`) is reachable only through `flatpak-spawn --host`, which needs the user's own opt-in `flatpak override --talk-name=org.freedesktop.Flatpak`: never put that in the manifest.
- **Flathub policy:** manifests must NOT contain AI-generated content and AI must not open the submission PR. Do not write a Flathub manifest or submission text for the owner; `docs/FLATHUB.md` explains the process.

## Open items

- OAuth 2.0 auth cannot be edited yet (imported settings are kept as they are).
- Folders in the tree cannot be collapsed (so no chevrons).
- Key/value table cells and checkboxes are std `LineEdit`/`CheckBox`, so they cannot be made borderless or recoloured; doing that means replacing `VarLineEdit`'s field with a custom one.
- Find matches and text selection in the response panel use the accent colour (std `TextEdit` offers no property).
- `family.slint` is v11 and `controls.slint` v9 here (selection tint, `Family.method-color`, `TabItem` count/dot, `GhostButton`): copy them back into FastHello and the other apps.
- The Flatpak asks for `--share=network` and `--filesystem=home` (typed paths); Flathub would likely want the second one dropped.

