# oray-tools

Command-line control of Oray smart plugs. Auth via the device's API, persist
tokens in a local config file, and switch plugs from a terminal.

## Features

- `login` — authenticate with account/password and persist tokens; handles the
  SMS verification flow used when registering a new trusted device
- `refresh` — renew tokens with the saved `refresh_token`
- `plug status / on / off` — query and switch plugs (add `--refresh-on-expired`
  to refresh the token and retry when the server reports `TOKEN_EXPIRED`)
- `plug add / remove / list` — manage multiple plugs (each name maps to a device SN)
- Per-operation `--index` for multi-port plugs, global `--config`/`--clientid`
- Machine-local trusted client ID (persisted, no hardcoded value)

## Installation

### NixOS

The project provides a Nix flake (`x86_64-linux`, `aarch64-linux`).

Run without installing:

```
nix run github:desktop-tools-which-may-be-useful/oray-tools -- plug status
```

Install into the user profile:

```
nix profile install github:desktop-tools-which-may-be-useful/oray-tools
```

Add it to your system configuration:

```nix
# flake.nix
{
  inputs.oray-tools.url = "github:desktop-tools-which-may-be-useful/oray-tools";
  outputs = { self, nixpkgs, oray-tools, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [
            oray-tools.packages.${pkgs.system}.default
          ];
        })
      ];
    };
  };
}
```

### Termux (Android)

Binaries are cross-compiled for `aarch64`, `arm` (armeabi-v7a) and `x86_64`
and published as an apt repository at `/termux` on GitHub Pages:

```
echo "deb [trusted=yes] https://desktop-tools-which-may-be-useful.github.io/oray-tools/termux stable main" > $PREFIX/etc/apt/sources.list.d/oray-tools.list
pkg update
pkg install oray-tools
```

The repository is currently unsigned, so the source uses `[trusted=yes]`.

### Debian / Ubuntu

Binaries are built for `amd64`, `arm64` and `armhf` and published as a standard
apt repository at `/debian` on GitHub Pages:

```
echo "deb [trusted=yes] https://desktop-tools-which-may-be-useful.github.io/oray-tools/debian stable main" > /etc/apt/sources.list.d/oray-tools.list
apt-get update
apt-get install oray-tools
```

The repository is currently unsigned, so the source uses `[trusted=yes]`.

### Windows

A self-contained `x86_64` executable is cross-compiled with the GNU toolchain
(static CRT, no extra DLLs) and published at:

```
https://desktop-tools-which-may-be-useful.github.io/oray-tools/windows/
```

Download `oray-tools-<version>-x86_64.exe` and run it from PowerShell or CMD.
Windows SmartScreen may warn about the unsigned binary on first run — choose
*More info > Run anyway*. (ARM64 Windows is not built yet; the
`aarch64-pc-windows-gnu` rust-std was removed from the stable toolchain.)

### Machine-readable release index

Binaries live in [GitHub Releases](https://github.com/desktop-tools-which-may-be-useful/oray-tools/releases);
the Pages site only hosts lightweight indexes and apt metadata. JSON endpoints
(handy for nvfetcher-style update checkers):

```
https://desktop-tools-which-may-be-useful.github.io/oray-tools/manifest.json   # manifest of the build just published
https://desktop-tools-which-may-be-useful.github.io/oray-tools/latest.json     # alias of manifest.json (latest build)
https://desktop-tools-which-may-be-useful.github.io/oray-tools/unstable.json   # alias of manifest.json (unstable build)
https://desktop-tools-which-may-be-useful.github.io/oray-tools/releases.json   # formal releases, newest first
```

Each manifest asset entry has `filename`, `url`, `size` and `sha256`, with
`url` pointing at the GitHub Release download link. `releases.json` lists
formal `v<version>` releases with their `manifest_url`.

The workflow distinguishes two kinds of release (the `workflow_dispatch`
inputs on the build action control this):

- **Formal release** (`release: true`, with an optional `version`): stable
  `v<version>` tag, kept forever.
- **Unstable build** (every push): fixed `unstable` tag, always points at the
  newest build so there is always a fresh distribution to fetch.

The apt repositories and the Windows download page are served from the Pages
deploy artifact (git never stores binaries), so each site only carries the
latest `.deb`/`.exe` while history lives in GitHub Releases.

## Usage

```
oray-tools login <account> <password>      # first run on a device may prompt for an SMS code
oray-tools refresh                         # renew tokens
oray-tools tokens                          # show token info and expiry
oray-tools plug add <name> <sn>            # register a plug by device SN
oray-tools plug status [name] [--index N]  # --refresh-on-expired: refresh+retry on TOKEN_EXPIRED
oray-tools plug on <name>                  # or: plug off
oray-tools plug list
oray-tools logout
```

`oray-tools <COMMAND> --help` shows command-specific options.

## Configuration

Config is stored in `$XDG_CONFIG_HOME/oray-tools/config.toml`
(`~/.config/oray-tools/config.toml`). It holds account credentials, tokens,
the trusted client ID, and plug SNs:

```toml
[account]
account = "..."
password_md5 = "..."

[client]
clientid = "..."          # generated UUID v4, used as the trusted Ex-ClientId

[token]
access_token = "..."
refresh_token = "..."
refresh_expires = ...

[plugs.main]
sn = "560056660997"

[server]
# api_base    = "https://api-std.sunlogin.oray.com"   # defaults
# slapi_base  = "https://slapi.oray.net"
```

Use `--config <path>` to point at a different file and `--clientid <id>` to
override the trusted client ID for a single run.

## Development

The project is a Cargo workspace with two crates:

- `crates/oray-core` — the protocol layer only, no filesystem/CLI surface.
  `AuthApi` and `PlugApi` are stateless HTTP clients (login/refresh/
  SMS-verification and device status/switch). The `reqwest` client, network
  errors (`oray_core::Error`) and all state are owned by the caller.
- `crates/oray-cli` — the `oray-tools` binary: clap argument parsing, command
  dispatch, persisted config (`config.rs`), token lifecycle and client-id
  management (`token.rs`). It injects a shared HTTP client into the core APIs
  and owns every side effect.

Dependencies flow one way only: `oray-cli → oray-core`. Build locally with
`cargo build` (the workspace `default-members` builds only the CLI).
Cross-compilation for the published targets (Termux/Debian/Windows) happens
in the release workflow.
