# oray-tools

Command-line control of Oray (Sunlogin) devices. The CLI is a thin wrapper
over the Oray cloud APIs: only authentication material is stored locally;
every device list/info/status is fetched live from the cloud on each command.

## Features

- `auth login` — authenticate with account/password and persist tokens;
  handles the SMS-verification flow used when registering a new trusted device
- `auth refresh / status / logout` — renew tokens, show expiry, clear local state
- `wakeup` — **开机设备** (smart plugs / power hardware), from `/wakeup/devices`:
  - `list`, `info <sn>`, `rename`, `memo`
  - `plug status / on / off [--index N]` — query and switch an outlet
  - `plug logs` — status-change history (paged, or filtered by `--since`)
  - `plug timer list/add/remove` and `plug countdown status/start/stop`
  - `plug led on|off`, `plug power-on-restore <0|2>`
- `remote` — **远程设备** (PCs / phones), from `/remotes`:
  - `list`, `info <id>`, `status <id>`, `rename`, `memo`
- Machine-readable output: every command accepts `--json`
- Debug output: every command accepts `--verbose` (raw request/response on stderr)
- `--refresh-on-expired` on `wakeup`/`remote` refreshes the token and retries
  once when the server reports `TOKEN_EXPIRED`
- Machine-local trusted client ID (persisted, no hardcoded value)

## Installation

### NixOS

The project provides a Nix flake (`x86_64-linux`, `aarch64-linux`).

Run without installing:

```
nix run github:desktop-tools-which-may-be-useful/oray-tools -- wakeup list
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

Authentication (stored locally):

```
oray-tools auth login <account> <password>   # first run on a device may prompt for an SMS code
oray-tools auth refresh                      # renew tokens
oray-tools auth status                       # show token info and expiry (--json)
oray-tools auth logout                       # clear saved tokens and account
```

Wakeup devices — smart plugs / power hardware (all data from the cloud):

```
oray-tools wakeup list                        # list devices
oray-tools wakeup info <sn>                   # device details
oray-tools wakeup rename <sn> <new-name>      # rename (keeps the memo)
oray-tools wakeup memo <sn> <text>            # set the memo/备注 (keeps the name)

oray-tools wakeup plug status <sn> [--index N]            # query outlet state
oray-tools wakeup plug on <sn> [--index N]                # switch on
oray-tools wakeup plug off <sn> [--index N]               # switch off
oray-tools wakeup plug logs <sn> [--since 2h|--page N]    # status history
oray-tools wakeup plug timer list <sn>                    # list timers
oray-tools wakeup plug timer add <sn> --time 480 --action 1 --repeat 31  # LOCAL 08:00, Mon-Fri (bit0=Mon..bit6=Sun, 0=once); plug stores UTC, tool converts
oray-tools wakeup plug timer remove <sn> <timer-id>
oray-tools wakeup plug timer enable <sn> <timer-id>       # activate a timer
oray-tools wakeup plug timer disable <sn> <timer-id>      # pause a timer (kept, inactive)
oray-tools wakeup plug countdown status <sn>              # show running countdown
oray-tools wakeup plug countdown start <sn> --count 600 --action 0
oray-tools wakeup plug countdown stop <sn>
oray-tools wakeup plug led <sn> on|off                    # LED indicator
oray-tools wakeup plug power-on-restore <sn> <0|2>        # state after power loss
```

Remote devices — PCs / phones (all data from the cloud):

```
oray-tools remote list                       # list remotes
oray-tools remote info <id>                  # extended detail
oray-tools remote status <id>                # online state / last seen
oray-tools remote rename <id> <new-name>     # rename (keeps the memo)
oray-tools remote memo <id> <text>           # set the memo (keeps the name)
```

Every command accepts `--json` for machine-readable output and `--verbose`
to print the raw HTTP request/response on stderr. Add `--refresh-on-expired`
to any `wakeup`/`remote` command to auto-refresh the access token and retry
once when the server reports `TOKEN_EXPIRED`.

`oray-tools <COMMAND> --help` shows command-specific options.

Example:

```
$ oray-tools wakeup list --json
{
  "devices": [
    {
      "device_id": 900001,
      "sn": "100000000001",
      "name": "Demo Smart Plug",
      "device_type": "sl_smartplug",
      "outletcount": 1
    }
  ]
}
```

## Configuration

Config is stored in `$XDG_CONFIG_HOME/oray-tools/config.toml`
(`~/.config/oray-tools/config.toml`). Only authentication material lives
there — no device data is cached:

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

[server]
# api_base    = "https://api-std.sunlogin.oray.com"   # defaults
# slapi_base  = "https://slapi.oray.net"

# Timezone of the plug for timer scheduling, same format as --tz
# (e.g. "+08:00" for China, "-05:00", or plain minutes like "480").
# When unset the CLI falls back to the machine's local offset and warns.
tz = "+08:00"
```

Use `--config <path>` to point at a different file and `--clientid <id>` to
override the trusted client ID for a single run. `--tz <offset>` overrides the
timezone for a single run and accepts the same formats as the config value
(e.g. `--tz +8`, `--tz -05:30`, or `--tz 480`).

## Development

The project is a Cargo workspace with two crates:

- `crates/oray-core` — the protocol layer only, no filesystem/CLI surface.
  Stateless HTTP clients over the Oray cloud APIs:
  - `auth` — login/refresh/SMS-verification token flow
  - `wakeup` — `/wakeup/devices` listing (`WakeupApi`)
  - `plug` — smart-plug controls on `slapi.oray.net` (`PlugApi`)
  - `remote` — remote devices on `api-std` (`RemoteApi`)
  - `output` — verbose request/response logging switch
  Network errors (`oray_core::Error`) and all state are owned by the caller.
- `crates/oray-cli` — the `oray-tools` binary: clap argument parsing, command
  dispatch, persisted config (`config.rs`), token lifecycle and client-id
  management (`token.rs`). It injects a shared HTTP client into the core APIs
  and owns every side effect.

Dependencies flow one way only: `oray-cli → oray-core`. Build locally with
`cargo build` (the workspace `default-members` builds only the CLI).
Cross-compilation for the published targets (Termux/Debian/Windows) happens
in the release workflow.
