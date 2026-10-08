# GoldCap Companion

A small tray app for Windows and macOS that keeps the [GoldCap](https://github.com/goldcap-gg/goldcap-addon)
addon's auction-house prices fresh, and — if you pair it with your account —
carries your own sales into your profit ledger on [goldcap.gg](https://goldcap.gg).

This repository is the app itself: the source the published installers are built
from, and the workflow that builds them. It is here so you can read what you are
about to run instead of taking our word for it.

**You do not need this app.** The addon works on its own — prices ship inside it
and refresh with every addon release. The companion only makes them hourly
instead of per-release. If you would rather not run an installer, install the
addon and stop there.

Download: **[goldcap.gg/downloads](https://goldcap.gg/downloads)**

## What it does

Every 30 minutes, or when you press "Sync now":

1. Fetches a price string for your realm from the public API.
2. Writes it into `Interface/AddOns/GoldCap_AppData/AppData.lua`. The addon picks
   it up on your next login or `/reload`.
3. If you paired it with a code from your account page, it also reads the GoldCap
   addon's own saved data and sends your sales, your listings and the prices you
   saw to your ledger.

Its window shows those three stages separately, so you can see which one works
and which does not.

## Everything it talks to

The complete list. Every address is ours, and nothing else is contacted.

| Request | What for |
| --- | --- |
| `GET api.goldcap.gg/v1/addon/import-string` | the price string for your realm |
| `GET api.goldcap.gg/v1/addon/realms`, `/v1/addon/resolve-realm` | realm names |
| `POST api.goldcap.gg/v1/companion/claim` | trades your pairing code for a token |
| `POST /v1/ledger/upload`, `/v1/live-observations`, `/v1/owned-lots`, `/v1/addon/item-names` | your own rows, from the addon's saved data |
| `GET /v1/lists/companion`, `/v1/ledger/summary/companion` | what the window shows you |
| `GET goldcap.gg/downloads/updater.json` | checking for a new version, every four hours — unless you turn that off in Settings and check with the button instead |

Every `POST` in that list is refused by the app itself until you pair it, so an
unpaired companion only ever reads. There is no usage tracking of any kind. The
only address it can open in your browser is `goldcap.gg`, and that limit is
enforced by the app's own permissions file rather than by good intentions.

On disk it writes two things: the price file in your `AddOns` folder, and its own
settings and log in your user profile. It reads the GoldCap addon's saved
variables. It never touches the game's memory or its process.

On Windows the installer is an `.msi`. It installs the app into Program Files, so
Windows asks for administrator rights once while it installs, and again when you
accept an update. A copy installed from the older `-setup.exe` installer keeps
updating from that installer, for your user only, and needs no reinstall.

**WoW: Forever.** If you also play WoW: Forever, the Companion finds that install next to retail. After each /reload it sends your Forever auction house scan to goldcap.gg (only the scan: never your ledger or characters), and it writes every player's Forever prices into the Forever install's `GoldCap_AppData`. It tells the two games apart by what the addon writes into its own save file, not by folder names. Retail works exactly as before.

## Checking the file you downloaded

1. **Compare the hash.** Each release's SHA-256 is published at
   [goldcap.gg/downloads](https://goldcap.gg/downloads) under "Checksums", written
   by the build itself. The file name carries the version, for example on
   Windows: `Get-FileHash .\GoldCap-Companion_1.18.0_x64.msi`.
   On macOS: `shasum -a 256 GoldCap-Companion_1.18.0_universal.dmg`.
2. **Or have it scanned.** Upload the file itself to
   [virustotal.com](https://www.virustotal.com) under File, or search there for
   its SHA-256. The URL tab only checks the address against blocklists, not the
   file. Unsigned installers can collect a generic machine-learning hit or two;
   that is the missing signature, not a finding about behaviour.
3. **Or build your own** and trust none of the above. With a Rust toolchain
   installed:

   ```bash
   cargo install tauri-cli --version "^2"
   cargo tauri build
   ```

Windows says "unknown publisher" because the installer is not code-signed yet —
a certificate we have not bought, not something about the build. Updates, on the
other hand, are signed: the app verifies a signature against the key built into
it and refuses anything that does not match.

## Licence

Source-available: read it, audit it, build it for yourself. Redistributing it, or
reusing the code elsewhere, needs permission — see [LICENSE](LICENSE).

## Questions, bugs and ideas

Open an issue here, or post on the [GoldCap Discord](https://goldcap.gg/discord):
**#help** for questions, **#bug-reports** when something is broken,
**#feature-requests** for what you would like it to do. Both places are read.
For anything you would rather not post in public: support@goldcap.gg.
