// Every state the Status and Wizard screens can be in, as the shapes get_status/get_config/
// detect_installs actually return. This file is the manual check-list: if a state is not here, it
// has not been looked at.

const NOW = Math.floor(Date.now() / 1000);

const configured = {
  region: "eu",
  realmSlug: "dentarg",
  wowRetailPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
  foreverRootPath: "",
  retailEnabled: true,
  foreverEnabled: false,
  intervalMinutes: 30,
  launchAtStartup: true,
  autoUpdate: true,
  companionToken: "tok",
};

const stage = (state, at, detail, error = null) => ({ state, at, detail, error });

export const FIXTURES = {
  "unconfigured (wizard)": {
    config: { ...configured, realmSlug: "", wowRetailPath: "", retailEnabled: false, foreverEnabled: false, companionToken: "" },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    foreverRoot: "",
    game: { region: "eu", realmNames: ["Tarren Mill", "Ravencrest"] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  "wizard: no install found": {
    config: { ...configured, realmSlug: "", wowRetailPath: "", retailEnabled: false, foreverEnabled: false, companionToken: "" },
    detectedPath: "",
    foreverRoot: "",
    game: { region: "eu", realmNames: [] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  "wizard: install but no realms": {
    config: { ...configured, realmSlug: "", wowRetailPath: "", retailEnabled: false, foreverEnabled: false, companionToken: "" },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    foreverRoot: "",
    game: { region: "eu", realmNames: [] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  // The default production first run, not an edge case: Config::load_or_init
  // auto-detects wowRetailPath/foreverRootPath and persists them before the
  // wizard ever opens, so a wholly-empty config is actually the rare case.
  // This one also pairs a config the user never finished — companionToken
  // already set, interval and startup already changed from their defaults,
  // Retail already turned on — before an earlier session hit "Later" (or
  // just quit) with the realm still unresolved. The wizard reopening on
  // realmSlug being empty must not wipe any of that.
  "wizard: upgrade with a token": {
    config: {
      region: "eu",
      realmSlug: "",
      wowRetailPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
      foreverRootPath: "",
      retailEnabled: true,
      foreverEnabled: false,
      intervalMinutes: 45,
      launchAtStartup: false,
      autoUpdate: false,
      companionToken: "tok",
    },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    foreverRoot: "",
    game: { region: "eu", realmNames: ["Tarren Mill", "Ravencrest"] },
    status: {
      configured: false, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 45, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  // A player who has only WoW: Forever installed — the wizard's Games step
  // finds only a Forever client, and the whole flow never asks for a realm.
  "wizard: forever-only, first run": {
    config: {
      region: "eu",
      realmSlug: "",
      wowRetailPath: "",
      foreverRootPath: "",
      retailEnabled: false,
      foreverEnabled: false,
      intervalMinutes: 30,
      launchAtStartup: true,
      autoUpdate: true,
      companionToken: "",
    },
    detectedPath: "",
    foreverRoot: "C:\\Program Files (x86)\\World of Warcraft",
    game: { region: "eu", realmNames: [] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Retail is off"),
      addon: stage("notConnected", null, "Retail is off"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  // Retail and Forever were found on two entirely different drives — the case this whole
  // feature exists for. The wizard's Games step must offer both independently, each with its own
  // path, even though neither folder is anywhere near the other.
  "wizard: two drives, first run": {
    config: {
      region: "eu",
      realmSlug: "",
      wowRetailPath: "",
      foreverRootPath: "",
      retailEnabled: false,
      foreverEnabled: false,
      intervalMinutes: 30,
      launchAtStartup: true,
      autoUpdate: true,
      companionToken: "",
    },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    foreverRoot: "D:\\Games\\World of Warcraft",
    game: { region: "eu", realmNames: ["Tarren Mill"] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  // Both a Retail and a Forever client sit under the same WoW folder, plus a
  // Classic Era install the wizard shows but cannot turn on.
  "wizard: both games, plus an unsupported one": {
    config: { ...configured, realmSlug: "", wowRetailPath: "", retailEnabled: false, foreverEnabled: false, companionToken: "" },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    foreverRoot: "C:\\Program Files (x86)\\World of Warcraft",
    classicEraFound: true,
    game: { region: "eu", realmNames: ["Tarren Mill"] },
    status: {
      configured: false, paired: false, retailEnabled: false, foreverEnabled: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  "everything works": {
    config: { ...configured, foreverEnabled: true, foreverRootPath: "C:\\Program Files (x86)\\World of Warcraft" },
    // Settings → "Check for updates" finds this one; in every other fixture
    // the check answers that this is the latest version.
    update: { version: "1.15.1", kind: "pending", action: "Install and restart" },
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1560, intervalMinutes: 30, version: "1.15.0",
      // The whole-market payload the last good tick wrote. get_status sends null for both
      // without one; the fixtures that leave them out render the same.
      marketItems: 9439, marketTs: NOW - 2400,
      prices: stage("ok", NOW - 240, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 240, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 240, "142 rows sent · queue empty"),
      games: [
        {
          folder: "_classic_beta_", game: "forever",
          market: "us-beta-classic-beta-pve-2-horde", realm: "Classic Beta PvE 2", faction: "Horde",
          lastSentAt: NOW - 720, sentItems: 2310, crowdItems: 1974, crowdTs: NOW - 600, crowdWrittenAt: NOW - 60,
        },
      ],
    },
  },

  // A Forever-only player: no Retail card at all, just the Forever card and
  // its own reminder about when a scan actually reaches disk.
  "forever-only: status": {
    config: {
      region: "eu",
      realmSlug: "",
      wowRetailPath: "",
      foreverRootPath: "C:\\Program Files (x86)\\World of Warcraft",
      retailEnabled: false,
      foreverEnabled: true,
      intervalMinutes: 30,
      launchAtStartup: true,
      autoUpdate: true,
      companionToken: "tok",
    },
    status: {
      configured: true, paired: true, retailEnabled: false, foreverEnabled: true, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: NOW + 900, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Retail is off"),
      addon: stage("notConnected", null, "Retail is off"),
      ledger: stage("notConnected", null, "Retail is off"),
      games: [
        {
          folder: "_classic_beta_", game: "forever",
          market: "us-beta-classic-beta-pve-2-horde", realm: "Classic Beta PvE 2", faction: "Horde",
          lastSentAt: NOW - 720, sentItems: 2310, crowdItems: 1974, crowdTs: NOW - 600, crowdWrittenAt: NOW - 60,
        },
      ],
    },
  },

  // A Forever-only player who has never had a scan accepted yet — no market,
  // no crowd line, just the "no scan uploaded yet" state and the reminder.
  "forever-only: no scan yet": {
    config: {
      region: "eu",
      realmSlug: "",
      wowRetailPath: "",
      foreverRootPath: "C:\\Program Files (x86)\\World of Warcraft",
      retailEnabled: false,
      foreverEnabled: true,
      intervalMinutes: 30,
      launchAtStartup: true,
      autoUpdate: true,
      companionToken: "",
    },
    status: {
      configured: true, paired: false, retailEnabled: false, foreverEnabled: true, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: NOW + 1800, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "Retail is off"),
      addon: stage("notConnected", null, "Retail is off"),
      ledger: stage("notConnected", null, "Retail is off"),
      games: [{ folder: "_classic_beta_", game: "forever" }],
    },
  },

  // A retail-only player — the common case today, and the one that must
  // render byte-for-byte what it always has: three stage rows, no Forever
  // card, no games list.
  "retail-only: status": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1560, intervalMinutes: 30, version: "1.15.0",
      marketItems: 9439, marketTs: NOW - 2400,
      prices: stage("ok", NOW - 240, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 240, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 240, "142 rows sent · queue empty"),
    },
  },

  "syncing right now": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: true, nextTickAt: NOW + 1800, intervalMinutes: 30, version: "1.15.0",
      marketItems: 9439, marketTs: NOW - 5400,
      prices: stage("ok", NOW - 1800, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 1800, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 1800, "142 rows sent · 8 queued"),
    },
  },

  "prices broken": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 600, intervalMinutes: 30, version: "1.15.0",
      prices: stage("broken", null, "Could not reach goldcap.gg", "unexpected status 500"),
      addon: stage("ok", NOW - 7200, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 7200, "142 rows sent · queue empty"),
    },
  },

  "addon not installed": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 900, intervalMinutes: 30, version: "1.15.0",
      prices: stage("ok", NOW - 120, "Fetched from goldcap.gg"),
      addon: stage("broken", null, "The GoldCap addon is not installed"),
      ledger: stage("notConnected", null, "No ledger yet — launch WoW with GoldCap"),
    },
  },

  "not paired": {
    config: { ...configured, companionToken: "" },
    status: {
      configured: true, paired: false, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 300, intervalMinutes: 30, version: "1.15.0",
      prices: stage("ok", NOW - 60, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 60, "Written to GoldCap_AppData"),
      ledger: stage("notConnected", null, "Not paired — nothing is uploaded"),
    },
  },

  "game idle for a week": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1200, intervalMinutes: 30, version: "1.15.0",
      prices: stage("ok", NOW - 300, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 300, "Written to GoldCap_AppData"),
      ledger: stage("notConnected", null, "No ledger yet — launch WoW with GoldCap"),
    },
  },

  "upload refused": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 400, intervalMinutes: 30, version: "1.15.0",
      prices: stage("ok", NOW - 180, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 180, "Written to GoldCap_AppData"),
      ledger: stage("broken", NOW - 90000, "Upload was refused", "unexpected status 503"),
    },
  },

  "addon has no file yet": {
    config: configured,
    status: {
      configured: true, paired: true, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 700, intervalMinutes: 30, version: "1.15.0",
      prices: stage("ok", NOW - 90, "Fetched from goldcap.gg"),
      addon: stage("notConnected", null, "No price file written yet"),
      ledger: stage("ok", NOW - 90, "142 rows sent · queue empty"),
    },
  },

  "configured, never synced, unpaired": {
    config: { ...configured, companionToken: "" },
    status: {
      configured: true, paired: false, retailEnabled: true, foreverEnabled: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1800, intervalMinutes: 30, version: "1.15.0",
      prices: stage("notConnected", null, "No sync yet"),
      addon: stage("notConnected", null, "No price file written yet"),
      ledger: stage("notConnected", null, "Not paired — nothing is uploaded"),
    },
  },
};
