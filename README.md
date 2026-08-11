# oray-tools

Command-line control of Oray smart plugs. Auth via the device's API, persist
tokens in a local config file, and switch plugs from a terminal.

## Features

- `login` — authenticate with account/password and persist tokens; handles the
  SMS verification flow used when registering a new trusted device
- `refresh` — renew tokens with the saved `refresh_token`
- `plug status / on / off` — query and switch plugs (auto-refreshes the token
  when the access token has expired)
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

Every published version is also archived under `releases/<version>/` on the
websites and indexed as JSON (handy for nvfetcher-style update checkers):

```
https://desktop-tools-which-may-be-useful.github.io/oray-tools/releases.json   # version list + all assets (newest first)
https://desktop-tools-which-may-be-useful.github.io/oray-tools/latest.json     # latest version manifest
https://desktop-tools-which-may-be-useful.github.io/oray-tools/releases/<version>/manifest.json
```

Each asset entry has `url`, `size` and `sha256`. The site is published from the
`gh-pages` branch, so all historical versions stay available.

## Usage

```
oray-tools login <account> <password>      # first run on a device may prompt for an SMS code
oray-tools refresh                         # renew tokens
oray-tools tokens                          # show token info and expiry
oray-tools plug add <name> <sn>            # register a plug by device SN
oray-tools plug status [name] [--index N]
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
