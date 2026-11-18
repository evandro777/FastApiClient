# FastApiClient

[![CI](https://github.com/evandro777/FastApiClient/actions/workflows/ci.yml/badge.svg)](https://github.com/evandro777/FastApiClient/actions/workflows/ci.yml)
[![Flatpak](https://github.com/evandro777/FastApiClient/actions/workflows/flatpak.yml/badge.svg)](https://github.com/evandro777/FastApiClient/actions/workflows/flatpak.yml)
[![AppImage](https://github.com/evandro777/FastApiClient/actions/workflows/appimage.yml/badge.svg)](https://github.com/evandro777/FastApiClient/actions/workflows/appimage.yml)
[![VirusTotal](https://github.com/evandro777/FastApiClient/actions/workflows/virustotal.yml/badge.svg)](https://github.com/evandro777/FastApiClient/actions/workflows/virustotal.yml)
[![made-for-linux](https://img.shields.io/badge/Linux-1f425f?logo=linux)](https://www.linux.org/)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-orange)](https://opensource.org/licenses/GPL-3.0)

A light and fast REST API client for Linux, built with Rust + [Slint](https://slint.dev) (software renderer, no GL). Projects, folders, environments, variables and request chaining, with your data kept in plain JSON files.

![FastApiClient](docs/screenshots/main.png)

Landing page: <https://evandro777.github.io/FastApiClient/> (published by the Flatpak workflow on the first tag).

## Features

- **Nested folders.** A folder can hold subfolders (to any depth); headers and auth are inherited project → folders → request. **New folder** with a folder (or a request inside one) selected creates a subfolder; with the project selected it creates a top-level folder. **New request** uses the same placement. Both ask for a name first: Create or Enter confirms; Cancel, Escape, clicking outside or submitting a blank name creates nothing. Right-click any folder for **New request**, **New folder**, Rename and Delete. Deleting a folder deletes everything inside it.
- **Variables.** Type `{{` in the URL, a header, a parameter, the body or the auth fields (or press Ctrl+Space) to pick from the active environment's variables; Up/Down and Enter or Tab choose, Esc closes. In single-line fields the cursor is taken to be where you last typed, so Ctrl+Space after moving it by hand inserts there.
- **Payload editing.** JSON and XML show line numbers, horizontal scrolling and live syntax warnings without blocking Send. JSON errors identify offending characters, including invisible controls and a missing straight closing quote before a line break. JSON comments are accepted; payloads containing `{{…}}` templates and XML with a DTD are marked as not checked. Enter keeps the current indentation and adds four spaces after opening delimiters. All paste paths preserve text. **Format**, beside the body selector for JSON/XML, formats the entire body with four spaces; invalid documents, templates and XML with a DTD, mixed content or `xml:space="preserve"` stay unchanged. In payload and environment editors, Tab inserts four spaces or indents selected lines; Shift+Tab unindents them. Ctrl+Z restores text and selection after indentation or Format. In payloads, press Escape, then Tab or Shift+Tab to move focus out of the editor (close variable suggestions first if open); in environments, Escape closes the dialog.
- **Functions.** The same list ends with ready-made functions (type `{{$`). `{{ $date }}` is now in ISO-8601 (local time); filters chain with `|`: `add('-60 days')` / `add('+1 month')` (second, minute, hour, day, week, month, year; month-end clamps), then one of `unix` (seconds) or `format('%Y-%m-%d')` (a [strftime](https://docs.rs/chrono/latest/chrono/format/strftime/index.html) mask). Example: `{{ $date | add('-1 month') | format('%d/%m/%Y') }}`. A mistake is reported as an undefined variable. A `|` inside a mask is not supported.
  - `{{ $response('Token request', '$.access_token', '$.expires_in') }}` takes a value from the JSON response of another request of the same project (found by name; pick one from the list). The optional third argument sets expiry: seconds (`3600`), a path into that response (`'$.expires_in'`), or an environment variable holding seconds (`'token_expires'`). Without it, the response is reused while it remains in the bounded cache; project definition or environment changes invalidate reuse. Missing, evicted or expired responses are fetched first; a failed, non-JSON or truncated refresh stops the dependent request instead of reusing an old token. Paths are `$.a.b` and `$.list[0].id`. Responses stay in memory, and only complete 2xx JSON responses are cached; manually sending the token request refreshes it too. Use it in auth, headers, URLs or bodies.
  - `{{ $prompt('Label', 'Description', 'Default value') }}` asks during Send only. Description and default are optional quoted strings; commas, Unicode, escaped quotes and backslashes are supported. OK accepts even an empty answer. The same label/description/default tuple reuses its answer across the Send and its dependent requests, then forgets it. Answers are inserted literally and never saved in request definitions. Cancel, Escape, clicking outside, changing selection or closing the window aborts remaining send work; dependencies already sent cannot be undone. Environment values are not recursively evaluated.
- **Request filtering.** **Filter requests** searches request names in the selected project, ignoring case. Matching requests keep their folders and order; unrelated folders disappear. Filtering keeps your current selection and unsaved edits, and arrow navigation uses the visible tree. Clearing restores all rows; switching projects clears the filter. The filter is not saved.
- **Response panel.** Tabs for the response body, its headers, and **Request**: the method, URL, headers and body actually sent, after variables, auth and inherited headers were applied (also shown when the request fails). The body toolbar offers **Raw data / Beautified**, **Copy**, **Open in editor** and **Find** (Ctrl+F; Enter or the arrows step through matches, Esc closes). Raw data preserves the original text without colors or Unicode decoding. Beautified formats and highlights JSON/XML and decodes JSON Unicode escapes (for example, `n\u00e3o` becomes `não`). Invalid JSON/XML stays unchanged with colors and a notice; HTML and unsupported formats stay raw.
- **Syntax colors.** JSON/XML in both editors use [VS Code Dark+ / Light+ token colors](https://github.com/microsoft/vscode/tree/main/extensions/theme-defaults/themes), following the appearance setting immediately. JSON keys, strings, numbers and booleans/null have distinct roles, as do XML tags, attributes and strings; punctuation keeps the app's foreground color. Highlighted responses scroll horizontally. Request colors stay visible while typing, with syntax refreshed after a short pause. Request coloring, manual request formatting and automatic response beautification apply up to **64 KiB (65,536 UTF-8 bytes)**, with formatted output also required to fit. **Beautified responses by default** in Preferences controls each new response; changing it also updates the current response using this limit. Larger responses start raw; choose Beautified to process the captured text (up to 1 MiB) in the background, with a 4 MiB formatted-output ceiling. Raw data remains available while processing, and switching back to Beautified reuses the current response's cached result. Editing, copying and search remain available for large bodies.
- **Projects.** The project is picked in the select box at the top of the sidebar (it has no row in the tree). **Manage** next to it holds: New project, Rename project, Delete project, Export, Import and **Global config** (the project's shared headers and auth). **Export** lists the projects with checkboxes and asks for a folder: each ticked project is written to its own `<name>.fastapiclient.json` there, and nothing is written if any of those files already exists. **Import** has a format selector, *FastApiClient* or *Insomnia V5.1*, and a file (type the path or use Browse…). **Preferences** at the bottom-left holds About.
- **Import from Insomnia** (*Manage* → *Import*, format *Insomnia V5.1*): reads an Insomnia **v5** export (`collection` or `spec`, YAML; one project per file) as a new project.
  - Mapped: nested folders, requests (method, URL, query, headers, JSON/XML/text/form/multipart bodies), folder and request auth (bearer, basic, OAuth2 `client_credentials`/`password`/`authorization_code`, API key as a header or query parameter), disabled rows, `{{ _.name }}` → `{{name}}`, and `{% response 'body', … %}` tags with a JSONPath filter → `{{ $response(…) }}` (XPath, header and other sources stay as written).
  - Environments: each Insomnia sub-environment becomes an environment holding the base values plus its own (the first one is active). Effective `GLOBAL_HEADERS` must agree across environments before they become shared project headers. Imports with differing headers stop without adding a project; replace differing credentials with a shared header template such as `Bearer {{token}}` and define `token` in each environment. Body media types are preserved unless an explicit Content-Type overrides them; multipart boundaries are generated when sending.
  - Literal `{% prompt 'Title', 'Label', 'Default', '', false, true %}` tags become `$prompt`: title → label, Insomnia label → description, default → default. Trailing default options may be omitted. Malformed/dynamic tags, nonempty storage keys, masked input and unsupported options stay as written. Imported prompts use the same per-Send answer lifetime.
  - Not imported: WebSocket, Socket.IO and gRPC requests, cookies, scripts, test suites, certificates, file fields of multipart bodies, bodies sent from a file (a shared collection must not be able to point Send at a file of yours; the same goes for a FastApiClient file, and you pick the file again in the request), other auth kinds (digest, AWS, Hawk…, imported as "no auth").
  - The export contains your tokens and passwords in clear text: keep it out of version control.
- **Interface.** English and Português (Preferences, or the system language), light and dark themes, one window (a second start raises the running one).

## Status

Project save failures stay pending and retry automatically, even after switching projects. An error names the affected project, and closing the window is blocked until pending writes succeed. Export without secrets removes environment values and Auth-tab secrets only: headers, URLs, queries and bodies still need review before sharing.

Failed project deletion keeps the project and its current edits visible, with an error so you can retry. A blocked close cancels Send and dismisses its prompts. Imports accept regular UTF-8 files up to 16 MiB. Invalid settings are backed up before using defaults; if that backup fails, startup stops with an error to protect the original file. Fix the reported file or directory problem and restart.

Cancelled sends cannot replace or clear newer cached responses, including responses fetched for dependencies. Body requests calculate Content-Length from the actual bytes or file, replacing supplied lengths. URL templates resolve before fragments are removed, so prompt labels containing `#` work and fragments in environment URLs are omitted before query parameters are added.

JSON numbers retain all digits in environment editing, saved projects, exports and chained responses, including IDs larger than a 64-bit integer. Environment, Import and Export dialogs close on outside clicks even when a text field has focus, as well as with Close or Escape. Closing the environment editor without saving discards its draft.

Imported and saved environment names are trimmed and made unique without dropping values or changing the active environment's values. Unnamed environments receive numeric names; choosing No environment always uses no environment values. Request headers override folder and project headers after template names resolve, and overridden values do not trigger prompts or dependent requests. Escape also closes Preferences, Manage and About after their controls receive focus.

Variable names inside header names remain case-sensitive; only the resolved HTTP names ignore case. Starting a manual HTTP refresh clears that request's cached response, so connection failures cannot leave an old token reusable. File bodies accept regular files only; pipes, directories and devices are rejected before sending.

Changing environments or saving environment changes cancels pending send work and clears its displayed response while keeping request edits. Redirects follow only within the original scheme, host and port, retaining Authorization and custom API-key headers there. Other destinations remain visible as 3xx responses with their Location header. Redirects that would replay a body with 307/308 also stay visible; uploads are never replayed automatically. Imported GLOBAL_HEADERS preserve variable-name case and convert Insomnia's `{{ _.name }}` syntax in header keys too.

Response dependencies used in header names resolve before header values or Auth-tab credentials. An inherited value that is then overridden cannot ask a prompt or send a dependency request. Responses fetched during one Send are reused across its preparation stages, including dependencies with zero expiry.

Generated multipart Content-Type and calculated Content-Length replace supplied values before prompts or response dependencies are evaluated. Native imports reject missing/null project payloads; an explicitly empty project is still valid. The response cache retains JSON bytes with a 32 MiB data budget (including project snapshots) and at most 256 responses, evicting older entries when full. Deleting a project clears its cached responses. If one Send needs more dependencies than the cache can retain, it stops with a cache-limit error instead of repeatedly fetching them.

Automatic payload validation skips bodies above 64 KiB. XML response formatting preserves leaf text, including surrounding spaces; mixed content, CDATA and `xml:space="preserve"` remain unchanged. Prompt defaults may contain quoted `}}`, including nested JSON objects. In JSON bodies, quoted prompt arguments also preserve URLs and literal `//` or `/* … */` text.

Drag the sidebar and request/response dividers to resize the panes; positions are remembered when the window closes. Focus a divider with Tab and use arrow keys for 8 px steps. Enter or a double-click restores its default (300 px sidebar, equal request/response panes). Resizing the window keeps both panes usable and restores your preferred sizes when space returns.

Early version. Tested on Linux Mint Cinnamon (X11). Not yet available: OAuth 2.0 editing (imported settings are kept as they are), collapsing folders in the tree.

## Install

### Flatpak

#### Install from file (no updates)

A `.flatpak` file downloaded from Releases is a one-off install (will not auto update). To get automatic updates, go to [Install with automatic updates](<#Install with automatic updates>).

Download `FastApiClient-<version>-x86_64.flatpak` from [Releases](https://github.com/evandro777/FastApiClient/releases), then execute:

```bash
flatpak install --user ~/Downloads/FastApiClient-<version>-x86_64.flatpak
```

#### Install with automatic updates

This app is not in Flathub yet. You need to add this repository as a remote. Every release is also published as a Flatpak remote, so `flatpak update` (or your software centre) keeps the app updated.

```bash
flatpak remote-add --system --if-not-exists fastapiclient https://evandro777.github.io/FastApiClient/fastapiclient.flatpakrepo
flatpak install --system fastapiclient io.github.evandro777.FastApiClient
```

The Flatpak has network access (to send requests) and access to your home folder (so a typed file path, for a file body or an import or export, opens as it does in the AppImage). Tighten it with `flatpak override --user --nofilesystem=home io.github.evandro777.FastApiClient`; then use **Browse…**, which goes through the file portal.

**`--system` vs `--user` matters for updates.** Linux Mint's Update Manager (and probably other software centres) only looks at *system-wide* Flatpaks; With `--user`, update by hand: `flatpak update --user`.

The remote is GPG-signed (system-wide installs refuse unsigned remotes), but the key lives in this same repository. It needs the Flathub remote for the runtime (`flatpak remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo`, with `--system` or `--user` to match).

### AppImage (with delta updates)

Download `FastApiClient-<version>-x86_64.AppImage` from [Releases](https://github.com/evandro777/FastApiClient/releases), then execute:

```bash
sudo chmod +x ~/Downloads/FastApiClient-<version>-x86_64.AppImage
~/Downloads/FastApiClient-<version>-x86_64.AppImage
```

It uses the fontconfig, xkbcommon and X11 libraries already on your system.

The AppImage does not update itself, but it has update information built in, so a tool such as [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) (`appimageupdatetool FastApiClient-*.AppImage`), [Gear Lever](https://github.com/mijorus/gearlever) or [AppManager](https://github.com/kem-a/AppManager) downloads only the changed parts of the next release.

## Build & Configurations

[Build and configurations information](BUILD.md)

## Contributing and releases

See [`CONTRIBUTING.md`](CONTRIBUTING.md) to build, test and send a change, and [`RELEASING.md`](RELEASING.md) for how a release is cut. CI (format, clippy, tests, MSRV, packaging checks, and the AppImage and Flatpak builds) runs on every pull request.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
