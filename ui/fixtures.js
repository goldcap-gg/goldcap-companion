// Every state the Status screen can be in, as the shapes get_status and
// get_config actually return. This file is the manual check-list: if a state
// is not here, it has not been looked at.

const NOW = Math.floor(Date.now() / 1000);

const configured = {
  region: "eu",
  realmSlug: "dentarg",
  wowRetailPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
  intervalMinutes: 30,
  launchAtStartup: true,
  companionToken: "tok",
};

const stage = (state, at, detail, error = null) => ({ state, at, detail, error });

export const FIXTURES = {
  "unconfigured (wizard)": {
    config: { ...configured, realmSlug: "", wowRetailPath: "", companionToken: "" },
    detectedPath: "C:\\Program Files (x86)\\World of Warcraft\\_retail_",
    game: { region: "eu", realmNames: ["Tarren Mill", "Ravencrest"] },
    status: {
      configured: false, paired: false, region: "eu", realmSlug: "",
      syncing: false, nextTickAt: null, intervalMinutes: 30, version: "1.2.0",
      prices: stage("notConnected", null, "Waiting for setup"),
      addon: stage("notConnected", null, "Waiting for setup"),
      ledger: stage("notConnected", null, "Waiting for setup"),
    },
  },

  "everything works": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1560, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 240, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 240, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 240, "142 rows sent · queue empty"),
    },
  },

  "syncing right now": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: true, nextTickAt: NOW + 1800, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 1800, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 1800, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 1800, "142 rows sent · 8 queued"),
    },
  },

  "prices broken": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 600, intervalMinutes: 30, version: "1.2.0",
      prices: stage("broken", null, "Could not reach goldcap.gg", "unexpected status 500"),
      addon: stage("ok", NOW - 7200, "Written to GoldCap_AppData"),
      ledger: stage("ok", NOW - 7200, "142 rows sent · queue empty"),
    },
  },

  "addon not installed": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 900, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 120, "Fetched from goldcap.gg"),
      addon: stage("broken", null, "The GoldCap addon is not installed"),
      ledger: stage("notConnected", null, "No ledger yet — launch WoW with GoldCap"),
    },
  },

  "not paired": {
    config: { ...configured, companionToken: "" },
    status: {
      configured: true, paired: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 300, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 60, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 60, "Written to GoldCap_AppData"),
      ledger: stage("notConnected", null, "Not paired — nothing is uploaded"),
    },
  },

  "game idle for a week": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1200, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 300, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 300, "Written to GoldCap_AppData"),
      ledger: stage("notConnected", null, "No ledger yet — launch WoW with GoldCap"),
    },
  },

  "upload refused": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 400, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 180, "Fetched from goldcap.gg"),
      addon: stage("ok", NOW - 180, "Written to GoldCap_AppData"),
      ledger: stage("broken", NOW - 90000, "Upload was refused", "unexpected status 503"),
    },
  },

  "addon has no file yet": {
    config: configured,
    status: {
      configured: true, paired: true, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 700, intervalMinutes: 30, version: "1.2.0",
      prices: stage("ok", NOW - 90, "Fetched from goldcap.gg"),
      addon: stage("notConnected", null, "No price file written yet"),
      ledger: stage("ok", NOW - 90, "142 rows sent · queue empty"),
    },
  },

  "configured, never synced, unpaired": {
    config: { ...configured, companionToken: "" },
    status: {
      configured: true, paired: false, region: "eu", realmSlug: "dentarg",
      syncing: false, nextTickAt: NOW + 1800, intervalMinutes: 30, version: "1.2.0",
      prices: stage("notConnected", null, "No sync yet"),
      addon: stage("notConnected", null, "No price file written yet"),
      ledger: stage("notConnected", null, "Not paired — nothing is uploaded"),
    },
  },
};
