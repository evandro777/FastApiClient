# <img src="packaging/io.github.evandro777.FastApiClient.svg" width="36" height="36" alt=""> FastApiClient

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

- **Build and organize requests.** Create projects and nested folders, then add requests with a method, URL, query parameters, headers, authentication and a body. Shared headers and authentication can be set for a project or folder.
- **Reuse values.** Define variables in environments and use them across requests. Requests can use dates, ask for input while sending, or take values from another request's JSON response, which supports common token and dependent-request workflows.
- **Edit and inspect data.** Edit JSON and XML with syntax colors, validation and formatting. View responses as received or in a formatted view, inspect response headers and the request that was sent, and search or copy response text. Recent responses remain available as you move between requests during the session.
- **Import and export.** Import projects from FastApiClient files or Insomnia v5 exports, and export projects to files. Review exported data before sharing because it may contain credentials.
- **Linux desktop app.** English and Portuguese interface, light and dark themes, and a resizable one-window layout.

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
