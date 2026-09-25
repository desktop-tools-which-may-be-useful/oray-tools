# oray-tools

Command-line control of Oray (Sunlogin) devices. The CLI is a thin wrapper
over the Oray cloud APIs: only authentication material is stored locally;
every device list/info/status is fetched live from the cloud on each command.

## Features

- `--interactive` — **显式**交互补全: prompt for the arguments the command
  line leaves out (hidden-echo password, saved account as the default, values
  validated while typing). It works in every group — `auth`, `wakeup`,
  `remote`, `wakeup plug timer/countdown` — and only ever behind this flag:
  without it a missing argument keeps clap's native error. `auth login
  <account> <password>` stays argument-driven and also handles the
  SMS-verification flow used when registering a new trusted device
- `auth login-sms <mobile>` — passwordless login with an SMS code
  (**手机验证码**): opens a browser for the slider captcha, requests the code
  and exchanges it for tokens (see [SMS login](#sms-login))
- `auth refresh / status / logout` — renew tokens, show expiry, clear local state
- `wakeup` — **开机设备** (smart plugs / power hardware), from `/wakeup/devices`:
  - `list`, `info <sn>`, `rename`, `memo`
  - `plug status / on / off [--index N]` — query and switch an outlet
  - `plug logs` — status-change history (paged, or windowed with `--since`/`--until`)
  - `plug timer list/add/remove` and `plug countdown status/start/stop`
  - `plug led on|off`, `plug power-on-restore <0|2>`
- `remote` — **远程设备** (PCs / phones), from `/remotes`:
  - `list`, `info <id>`, `status <id>`, `rename`, `memo`
- Machine-readable output: every command accepts `--json`
- Debug output: every command accepts `--verbose` (full request/response
  detail on stderr: method, URL, headers, request/response body). Sensitive
  values are masked by default; add `--trace-raw` to `--verbose` to see them
  verbatim
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
oray-tools auth login --interactive          # prompt for the arguments left out
oray-tools auth login <account> <password>   # first run on a device may prompt for an SMS code
oray-tools auth login-sms <mobile>           # passwordless: captcha in the browser + SMS code
oray-tools auth refresh                      # renew tokens
oray-tools auth status                       # show token info and expiry (--json)
oray-tools auth logout                       # clear saved tokens and account
```

### Interactive completion (`--interactive`)

Prompting is opt-in and explicit: add `--interactive` — anywhere on the
command line, in any command group — and every argument that is still
missing is asked for. Values already given are never asked again, so
`oray-tools auth login alice --interactive` asks only for the password.
The login method itself is never a choice: it is the subcommand
(`login` = account + password, `login-sms` = mobile number).

```
$ oray-tools auth login --interactive
Account (mobile or email) [alice@example.com]:
Password:
$ oray-tools wakeup rename --interactive
Device serial number (SN): SN1234567
New device name: bedroom plug
$ oray-tools remote rename --interactive
Remote id: 42
New device name: office pc
```

- Without the flag nothing ever asks: a missing argument is still clap's
  native `error: the following required arguments were not provided:
  <PASSWORD>` (exit 2), `--help` still documents `<ACCOUNT> <PASSWORD>` as
  required, and a bare `oray-tools auth` fails like the other subcommand
  groups (`wakeup`, `remote`): usage help and exit code 2.
- Values are checked while typing — a bad `--time`, an implausible mobile
  number or a non-numeric id is reported and asked for again.
- The password is read without echoing; the saved account is offered as the
  default for `login`, and for `login-sms` when it looks like a phone number.
- Prompts go to **stderr**, so `--json` output on stdout stays
  machine-readable.
- Without a terminal (a pipe, CI) `--interactive` fails immediately instead
  of hanging, naming the missing arguments and the concrete command that
  supplies them.

The split is deliberate: every human interaction — prompts, the SMS-code
question, the slider captcha — lives in `oray-cli` (`prompt.rs`,
`captcha.rs`), while `oray-core` stays a pure protocol layer of endpoints
and data exchange (enforced by `crates/oray-core/tests/purity.rs`, which
fails if interaction primitives show up in the core).

### SMS login

`auth login-sms <mobile>` logs in without a password, using the same flow as
the Sunlogin clients (a proxy capture of the Android client was used to derive
it):

1. A loopback page (`http://127.0.0.1:<port>/`) is served and opened in the
   browser with Aliyun's slider captcha (scene `1sdsal45`). Solve it; the page
   posts the captcha result back to the CLI. Use `--no-browser` to get the URL
   printed instead of opened, or `--captcha <token>` to supply a result
   obtained elsewhere.
2. The CLI asks the shield service for the code:
   `POST https://shield-api-v3.oray.com/seccode/mobile` with
   `plan_alias=sl-code-client-login` and
   `checksum = md5(plan_alias + mobile + "/seccode/mobile" + timestamp)`.
   The shield service rejects any request without a captcha result, which is
   why step 1 cannot be skipped.
3. The code you receive on the phone is exchanged for tokens:
   `POST https://api-std.sunlogin.oray.com/authorization`
   with `type=securecode, medium=sms, code-type=sl-code-client-login`.

```
oray-tools auth login-sms 12345678901                # prompts for the code
oray-tools auth login-sms 12345678901 --no-browser   # print the captcha URL
                                                     # instead of opening it
oray-tools auth login-sms 12345678901 --code 123456  # code you already have:
                                                     # skips captcha + SMS request
```

Tokens are stored like a password login, so `auth refresh`, `auth status` and
every device command work unchanged. The shield base URL is configurable as
`server.shield_base`.

Wakeup devices — smart plugs / power hardware (all data from the cloud):

```
oray-tools wakeup list                        # list devices
oray-tools wakeup info <sn>                   # device details
oray-tools wakeup rename <sn> <new-name>      # rename (keeps the memo)
oray-tools wakeup memo <sn> <text>            # set the memo/备注 (keeps the name)

oray-tools wakeup plug status <sn> [--index N]            # query outlet state
oray-tools wakeup plug on <sn> [--index N]                # switch on
oray-tools wakeup plug off <sn> [--index N]               # switch off
oray-tools wakeup plug logs <sn> [--since 2h] [--until 6h] [--page N] # status history
# --since/--until bound the window (ago like 2h/1d, or an absolute time like
# 2026-09-03 or 2026-09-03 09:00[:00]); a bare date runs to the day's end for
# --until. Absolute times are read in the plug's timezone (--tz / config tz,
# else machine local), and every printed time carries that zone (e.g. "... 10:07:18 UTC+08:00").
# The server has no time-window query, so the CLI locates the pages that can
# match (binary search) and filters locally.
oray-tools wakeup plug timer list <sn>                    # list timers
oray-tools wakeup plug timer add <sn> --time 08:00 --action 1 --repeat 31  # LOCAL 08:00, Mon-Fri (bit0=Mon..bit6=Sun, 0=once); minutes also accepted (--time 480); plug stores UTC, tool converts
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
to print the full request/response exchange (method, URL, headers, request and
response bodies) on stderr. Sensitive values are masked by default; add
`--trace-raw` to see them verbatim. Add `--refresh-on-expired` to any
`wakeup`/`remote` command to auto-refresh the access token and retry once
when the server reports `TOKEN_EXPIRED`.

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
# shield_base = "https://shield-api-v3.oray.com"      # sends SMS login codes

# Timezone of the plug for timer scheduling and for absolute `logs
# --since/--until` windows / displayed times, same format as --tz
# (e.g. "+8h" for China, "-5h", "+480min", "+08:00", "-08:20"; a sign is
# always required).
# When unset the CLI falls back to the machine's local offset and warns.
tz = "+08:00"
```

Use `--config <path>` to point at a different file and `--clientid <id>` to
override the trusted client ID for a single run. `--tz <offset>` overrides the
timezone for a single run and accepts the same formats as the config value
(e.g. `--tz +8h`, `--tz -05:30`, or `--tz +480min`).

## Development

The project is a Cargo workspace with two crates:

- `crates/oray-core` — the protocol layer only: endpoints, request/response
  shapes and data exchange. No filesystem/CLI surface, no output of its own
  and no human interaction — `tests/purity.rs` scans the crate's sources and
  fails if stdin/prompt/process/filesystem primitives appear there.
  Stateless HTTP clients over the Oray cloud APIs:
  - `auth` — password login, SMS-code login (`send_login_code`,
    `login_with_code`), the trusted-device verification flow and refresh
  - `wakeup` — `/wakeup/devices` listing (`WakeupApi`)
  - `plug` — smart-plug controls on `slapi.oray.net` (`PlugApi`)
  - `remote` — remote devices on `api-std` (`RemoteApi`)
  - `trace` — every call returns the parsed data plus the full request/response
    exchanges as a `Traced<T>` (or `TracedError` on failure); `redacted()`
    masks sensitive values for display
  Network errors (`oray_core::Error`) and all state are owned by the caller.
- `crates/oray-cli` — the `oray-tools` binary: clap argument parsing, command
  dispatch, persisted config (`config.rs`), token lifecycle and client-id
  management (`token.rs`), the interactive prompts (`prompt.rs`) and the
  loopback browser handshake for the login captcha (`captcha.rs` +
  `captcha_page.html`). It owns every presentation concern — `--json`, human
  text and the `--verbose`/`--trace-raw` request rendering — by consuming the
  traces the core returns. It injects a shared HTTP client into the core APIs
  and owns every side effect: everything a *person* does (typing a parameter,
  solving the slider) happens here, never in the core.

Dependencies flow one way only: `oray-cli → oray-core`. Build locally with
`cargo build` (the workspace `default-members` builds only the CLI).
Cross-compilation for the published targets (Termux/Debian/Windows) happens
in the release workflow.
