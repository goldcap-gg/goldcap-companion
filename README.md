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
| `GET goldcap.gg/downloads/updater.json` | checking for a new version |

Every `POST` in that list is refused by the app itself until you pair it, so an
unpaired companion only ever reads. There is no usage tracking of any kind. The
only address it can open in your browser is `goldcap.gg`, and that limit is
enforced by the app's own permissions file rather than by good intentions.

On disk it writes two things: the price file in your `AddOns` folder, and its own
settings and log in your user profile. It reads the GoldCap addon's saved
variables. It installs for your user only, with no administrator prompt, and it
never touches the game's memory or its process.

## Checking the file you downloaded

1. **Compare the hash.** Each release's SHA-256 is published at
   [goldcap.gg/downloads](https://goldcap.gg/downloads) under "Checksums", written
   by the build itself. On Windows: `Get-FileHash .\GoldCap-Companion-Setup.exe`.
   On macOS: `shasum -a 256 GoldCap-Companion.dmg`.
2. **Or have it scanned before you download it.** Paste the download URL into
   [virustotal.com](https://www.virustotal.com) → URL. Unsigned installers often
   collect a generic "heuristic" hit or two; that is the missing signature, not a
   finding about behaviour.
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
