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

`$XDG_DATA_HOME/FastApiClient/settings.toml` (defaults to `~/.local/share/FastApiClient`; in the Flatpak under `~/.var/app/<app-id>/data/`):

- `language`: `""` follows the system locale; otherwise a code such as `"pt_BR"`.
- `width`, `height`: the window size.
- `auto_beautify`: `true` (default) shows JSON/XML responses re-indented on arrival; also a checkbox in Preferences.
- `theme`: `""` follows the system's light or dark mode; otherwise `"light"` or `"dark"`. The **Appearance** box in Preferences sets it, and it is saved as soon as it changes.

(Edit: document the settings the app adds.)
