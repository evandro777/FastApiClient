# Build

## How it is built

- **Rust + Slint** with the winit backend and the **software renderer** (no GPU/GL context, which is where most of the memory of a toolkit app goes).
- **`crates/core`** has the logic with no UI code (settings today), so a CLI could reuse it; **`crates/app`** is the Slint UI and `main.rs`.
- **No async runtime.** Work runs in threads and is handed to the UI thread with `slint::invoke_from_event_loop`.

More detail for contributors is in [`AGENTS.md`](AGENTS.md).

## First build

Install a Rust toolchain and the libraries it links.

Debian/Ubuntu/Mint:

```bash
sudo apt install libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-dev libxcb1-dev rustup
rustup default stable
```

## How to run

```bash
cargo run --profile fast         # quick iteration build (no LTO)
cargo build --release            # thin LTO
cargo test
```

## From a release (Flatpak bundle)

Download `FastApiClient-<version>-x86_64.flatpak` from the [Releases](https://github.com/evandro777/FastApiClient/releases) page, then:

```bash
flatpak install --user ./FastApiClient-<version>-x86_64.flatpak
flatpak run io.github.evandro777.FastApiClient
```

For automatic updates use the Flatpak remote described in the [README](README.md#install) instead.

## Build the Flatpak yourself

App id: `io.github.evandro777.FastApiClient`.

```bash
sudo apt install flatpak-builder
flatpak install --user flathub org.freedesktop.Sdk//25.08 org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak/build.sh --devel      # installs io.github.evandro777.FastApiClient.Devel next to a release
flatpak run io.github.evandro777.FastApiClient.Devel
```

`flatpak/build.sh` without `--devel` builds the real id; `--bundle` also writes `flatpak/fastapiclient.flatpak`. Crates are vendored
into `flatpak/vendor` (git-ignored) because the sandboxed build has no network, and the build tree goes to
`~/.cache/fastapiclient-flatpak` (set `FASTAPICLIENT_FLATPAK_CACHE` to move it), so it stays out of the repository. The sandbox only gets the
sockets listed in `finish-args` of the manifest. Settings live in `~/.var/app/io.github.evandro777.FastApiClient/data/FastApiClient/`.

How the signed remote is built on GitHub, and how to make its key: [`docs/FLATPAK-SIGNING.md`](docs/FLATPAK-SIGNING.md).

## Build the AppImage yourself

```bash
appimage/build.sh             # needs appimagetool on PATH; writes FastApiClient-<version>-x86_64.AppImage (+ .zsync)
```

## Settings and data

JSON/XML payload validation runs after a 250 ms pause in typing and never blocks saving or sending. It checks syntax only, not schemas, and skips bodies above the shared 64 KiB UTF-8 limit before parsing. JSON accepts `//` and `/* … */` comments; unfinished block comments are errors. Payloads containing `{{…}}` expressions (including unfinished expressions) are not checked, and XML DTD processing is disabled. No variables or chained requests are resolved for validation.

Failed project writes remain in an app-local dirty-ID set and retry on each autosave and close, independently of editor changes or selection. The UI shows the first outstanding error and refuses a normal close until all pending projects save. Import reports a write failure instead of claiming durable success. Deleted projects are removed from the retry set without being recreated.

Project deletion removes the saved file before changing the workspace; failures preserve selection and the current draft and show a translated error. A missing saved file counts as already deleted. Close cancellation clears Send and its prompts even when a pending save keeps the window open. Imports open with Linux `O_NONBLOCK`, reject nonregular files, and bound the actual UTF-8 read to 16 MiB plus one byte, so pipes and devices cannot bypass the limit. If invalid settings cannot be backed up, settings loading returns an error and startup stops before defaults can overwrite the original; fix the reported file or directory problem and restart.

Chained responses are checked against a project snapshot, including active environment, values and request/header/auth definitions. A result from an older worker cannot match changed definitions. Refreshing with an unsuccessful, non-JSON or truncated response removes its prior cache entry and stops dependencies. Template delimiters inside quoted function arguments are ignored by the shared expression scanner. JSON comment removal preserves complete template expressions too, so comment markers in prompt arguments remain literal.

Each app Send shares the response cache through `Responses::for_send`, tied to its selection/window generation. Both dependency invalidation and response publication check that generation under the cache lock, so a cancelled worker cannot erase or replace a newer result even when project definitions are identical. A dependency cancelled during HTTP returns `Error::Cancelled` before caching its response. For body requests, Send discards supplied Content-Length headers and computes one from bytes or the opened file; the request log omits discarded lengths too. URL substitution precedes fragment removal and query construction, preserving `#` in prompt arguments and dropping fragments supplied by environment values.

The core crate enables serde_json's `arbitrary_precision` feature so JSON numbers keep their digits through environment editing, project persistence, native export/import and response substitution. Numeric IDs must not pass through floating-point conversion. Environment and transfer backdrops live inside the FocusScope that contains their inputs, matching Prompt, so outside clicks dismiss them while a text field has focus. Close, Escape and outside clicks discard an unsaved environment draft. After a debug build, `python3 dev/check-dialogs.py` verifies focused Environments, Export and Import dismissal on private Xvfb with canned data (requires `Xvfb`, `xdotool`, `xclip`, `xwd`, `ffmpeg` and `tesseract`); its window-only screenshots are under `target/ui-snapshots/dialogs-*`.

All three external project readers (native import, Insomnia import and saved project loading) call `Project::normalize_environments`. It trims names, uses numeric positions for unnamed entries, and applies the editor's case-insensitive suffix rule to collisions. The active entry is captured before renaming; empty or unknown active names mean no environment. Insomnia selects its first environment after normalization. Loaded repairs stay in memory until the next project save. Header preparation resolves names before choosing winners and evaluates only winning values, avoiding duplicate credentials, unnecessary prompts and dependency fetches; disabled rows and nonrecursive substitution retain their existing behavior. Preferences/Manage and About place both the card and backdrop inside their Escape focus scope. `dev/check-dialogs.py` also checks Escape after using the Appearance list and after Tab in About and Manage.

HTTP preparation walks enabled header rows directly: merging unresolved names would confuse case-sensitive variables such as `{{Name}}` and `{{name}}`. Manual HTTP refreshes and dependency refreshes use `Responses::invalidate` under the generation guard before sending, so network failures leave no old response reusable. File bodies open with Linux `O_NONBLOCK` and require regular-file metadata before constructing the HTTP body; network timeouts cannot protect a blocking file open.

Environment mutations share `reset_send` with request changes: it bumps the worker generation, drops pending dialog replies and clears response/log state without reloading editor values. No-op environment selection/value saves preserve the current send. Insomnia GLOBAL_HEADERS convert template keys without lowercasing their variable names; only literal keys are normalized before base/sub-environment merging. The unresolved-header merge compares template expressions exactly and literal keys without case.

ureq's automatic redirects are disabled because its Authorization policy does not protect custom API-key headers. HTTP follows at most ten redirects itself, retaining one 60-second budget and following only the original URL origin (scheme, host, effective port). The existing workspace `url` parser resolves relative Location values and normalizes default ports. Foreign origins, embedded credentials, and 307/308 responses requiring a body replay remain visible as 3xx responses. Redirected requests drop Cookie and Content-Length, matching the previous HTTP library behavior.

Header-name dependencies defer header values, auth and body evaluation until names resolve. `chain::Preparation` shares freshly fetched request names across the entire Send and re-prepares after each dependency; only those fresh responses bypass expiry. This avoids both overridden prompts/dependency fetches and stale values in dependencies discovered in a later stage.

Before substituting header values, preparation drops Content-Length for bodies and Content-Type for multipart bodies because Send replaces them. Native import requires a project payload in version 1 and a nonempty projects list in version 2; missing/null payloads and version 0 are rejected, while an explicit empty project still uses field defaults.

Responses retain validated JSON bytes in shared byte slices, not expanded JSON trees. Their context is a compact serialized project snapshot. A global 32 MiB budget covers stored JSON, snapshots and key bytes; a 256-response ceiling bounds map overhead. Oldest responses are evicted first, keeping the newly stored response when it fits. Lookup parses one captured response outside the cache lock. An evicted dependency already fetched in this Send produces `Error::CacheLimit`, avoiding a refetch loop. Successful project deletion bumps the send generation before clearing its cache, so late workers cannot recreate deleted entries. Snapshot page `cache-limit` covers the translated error.

Insomnia imports retain body media types through Content-Type headers, preserving explicit overrides and generated multipart boundaries. Effective GLOBAL_HEADERS are compared after merging base and sub-environment values; differing definitions are rejected with a localized explanation. Shared templates such as `Bearer {{token}}` remain supported. Export only removes environment values and Auth-model secrets, with an explicit warning to review other fields. The obsolete multi-project writer is removed; legacy version-2 import fixtures retain read compatibility.

Response XML formatting keeps leaf text verbatim and leaves mixed content, CDATA and `xml:space="preserve"` unchanged. Snapshot pages `save-error`, `import-headers`, `validation-limit`, `response-xml` and `export` cover these states.

The existing Slint editor handles payloads and environments: Tab inserts four spaces at the caret, or indents every selected line; Shift+Tab removes up to four leading spaces or one leading tab. A selection ending at the start of a line excludes that line. Ctrl+Z restores the previous text and selection after indentation. In JSON/XML request bodies, Enter keeps the line's indentation, adds four spaces after an opening delimiter and splits matching delimiters onto separate lines. Line numbers include empty and trailing lines, scroll vertically with the payload and remain fixed during horizontal scrolling. They never enter the saved or sent text. These changes do not affect environment or response editors, or response Beautify indentation.

All paste paths preserve text. The JSON/XML **Format** button formats the whole body using four spaces and returns focus to the editor. The formatter preserves JSON key order (including duplicate keys), numeric spelling, strings and comments; XML tags, leaf text, entities and declarations are kept verbatim. Invalid documents, templates, DTDs, mixed content, `xml:space="preserve"`, CDATA and XML whitespace character references are left unchanged. One Ctrl+Z restores the text and selection before Format; a no-op creates no undo entry. JSON diagnostics name raw line breaks/tabs/controls and show printable offending characters with their Unicode code, using source character columns; other errors retain the original parser message. No characters are automatically corrected.

JSON/XML request bodies and responses have syntax colors using `syntect` with its pure Rust regex engine. Grammars load lazily on a background thread; typing triggers highlighting after a 100 ms pause, and obsolete results are discarded. While typing, unchanged text keeps its colors and inserted text inherits a nearby token's color until parsing finishes, avoiding a whole-body flash to plain text. VS Code Dark+ / Light+ token colors follow the appearance setting immediately without reparsing. Grammar scopes distinguish JSON keys from strings, numbers from booleans/null, and XML tags from attributes and strings; punctuation uses the app's foreground color. Highlighted responses use monospace text with horizontal scrolling; headers and request logs retain wrapping. HTML is not highlighted. Native editing and selection remain in Slint, with native text displayed during input-method composition.

Request coloring, manual request formatting and automatic response beautification use a limit of 64 KiB (65,536 UTF-8 bytes, inclusive). Both original and formatted text must fit. Raw data preserves response whitespace and literal Unicode escapes without colors. Choosing Beautified manually bypasses the automatic limit for the captured response (up to 1 MiB), with formatted output bounded to 4 MiB. Formatting and coloring run together on the existing background worker; the toolbar shows “Beautifying…” and keeps Raw data available. Switching to Raw, changing requests or receiving another response invalidates pending results. Completed results are cached for the current response. Invalid JSON/XML shows original text with syntax colors and a notice; output overflow keeps Raw data. HTML and unsupported formats stay raw. Saved request text is never reformatted merely by opening it.

Request filtering uses `Workspace::filtered_tree` for rendering and arrow navigation. It matches request names only, using Unicode lowercase substring comparison, and preserves ancestors and node indexes. The filter is transient, clears on project changes, and refreshes rows without loading or saving editor values.

Request/folder creation uses Prompt kinds 10/11, shared by sidebar and folder context actions. Opening or dismissing the prompt leaves workspace selection and editor contents intact. Confirmation trims the name and rejects blanks before flushing the editor, creating, saving and selecting one item. Placement follows the selected folder (including nested folders), or the project when no folder is selected; Rename/F2 stays separate. Snapshot pages `new-folder`, `new-request` and `folder-menu` show the dialogs and folder actions. After a debug build, `python3 dev/check-creation.py` checks dismissal, confirmation, saved edits, placement and Rename/F2/Delete on private Xvfb with temporary projects and pointer targets derived from OCR/pixels; `--menus` skips the dismissal checks.

`$prompt('Label', 'Description', 'Default')` accepts one to three quoted strings and resolves only during Send. `chain::prepare_interactive` caches answers by the full parsed tuple across preparation passes and dependencies; noninteractive preparation leaves prompts unresolved. The worker bridges questions and resolved file confirmations through the Slint event loop and reply channels (`sending.rs`). Cancel or selection/window changes drop pending replies and stop remaining work without a network error. Answers are inserted literally, never saved or recursively evaluated. Insomnia literal prompt tags convert title/label/default with ordinary default options; storage keys, masking and unsupported variants remain untouched. No settings migration or cross-session prompt storage is used.

On extremely long unwrapped lines (32,000 physical pixels or more), selection uses accent-colored text without a filled background to avoid a Slint 1.18 software-renderer overflow. Copy and selection offsets are unchanged.

In payloads, press Escape first to move focus out with Tab or Shift+Tab (close variable suggestions first if open); in environments, Escape closes the dialog. These behaviors need no new settings.

After a debug build, `python3 dev/check-payload-editor.py` checks indentation, verbatim paste, explicit JSON/XML formatting, Enter, horizontal scrolling, selection, undo and variable completion on a private Xvfb display (requires `Xvfb`, `xdotool`, `xclip`, `xwd`, `ffmpeg` and `tesseract`). It saves window-only screenshots under `target/ui-snapshots/editor-*.png` and uses OCR to locate controls and verify caret visibility and the fixed gutter.

`$XDG_DATA_HOME/FastApiClient/settings.toml` (defaults to `~/.local/share/FastApiClient`; in the Flatpak under `~/.var/app/<app-id>/data/`):

- `language`: `""` follows the system locale; otherwise a code such as `"pt_BR"`.
- `width`, `height`: the window size.
- `sidebar_width`: preferred sidebar width in logical pixels (default `300`, minimum `260`). The workspace retains at least 584 px.
- `request_split`: preferred request fraction of the space below the request tabs (default `0.5`). Request and response retain at least 140 px and 180 px respectively, growing for visible controls. Window resizing clamps displayed sizes without changing these preferences. Both positions save on normal close alongside the window size.
- `auto_beautify`: `true` (default), labeled **Beautified responses by default** in Preferences. Formats and highlights JSON/XML and decodes JSON Unicode escapes when original and formatted text fit 64 KiB. Changing it also updates the current response using the automatic limit. The Raw data / Beautified toolbar choice applies only to the current response; the settings key and default are unchanged.
- `theme`: `""` follows the system's light or dark mode; otherwise `"light"` or `"dark"`. The **Appearance** box in Preferences sets it, and it is saved as soon as it changes.

The app-local `ui/divider.slint` shares pointer capture, window-coordinate drag tracking, keyboard arrows (8 px), reset (Enter/double-click), and focus feedback across both boundaries. The response uses two normal header rows: tabs/status, then mode/timing/size/actions. Find, errors, truncation and formatting notices add rows only while visible.

After a debug build, `python3 dev/check-panes.py` checks divider dragging, keyboard traversal/resizing, bounds, reset, window resizing, request switching and restart persistence with temporary settings/projects on private Xvfb. It requires Pillow and the existing keyboard harness tools. Snapshot pages `response-normal`, `response-find` and `response-notice` cover header states; set `OUT` to preserve different size/theme/language runs.
