// Vanilla JS, no bundler: the Tauri API is injected as `window.__TAURI__`
// because `app.withGlobalTauri` is `true` in tauri.conf.json.
const invoke = window.__TAURI__.core.invoke;

const form = document.getElementById("settings-form");
const regionEl = document.getElementById("region");
const realmSlugEl = document.getElementById("realmSlug");
const wowPathEl = document.getElementById("wowRetailPath");
const intervalEl = document.getElementById("intervalMinutes");
const launchAtStartupEl = document.getElementById("launchAtStartup");
const feedbackEl = document.getElementById("feedback");
const statusLineEl = document.getElementById("status-line");
const detectBtn = document.getElementById("detect-btn");
const browseBtn = document.getElementById("browse-btn");
const syncNowBtn = document.getElementById("sync-now-btn");
const gameRealmsLabel = document.getElementById("game-realms-label");
const gameRealmsEl = document.getElementById("gameRealms");

function fillForm(config) {
  regionEl.value = config.region;
  realmSlugEl.value = config.realmSlug;
  wowPathEl.value = config.wowRetailPath;
  intervalEl.value = String(config.intervalMinutes);
  launchAtStartupEl.checked = Boolean(config.launchAtStartup);
}

function showFeedback(message, isError) {
  feedbackEl.textContent = message;
  feedbackEl.classList.toggle("error", Boolean(isError));
  feedbackEl.classList.toggle("ok", !isError);
}

async function loadConfig() {
  try {
    const config = await invoke("get_config");
    fillForm(config);
    if (config.wowRetailPath) {
      await refreshGameSettings({ autofill: !config.realmSlug });
    }
  } catch (e) {
    showFeedback(`Failed to load settings: ${e}`, true);
  }
}

async function refreshStatus() {
  try {
    statusLineEl.textContent = await invoke("get_status_label");
  } catch {
    // Non-fatal — the status line is a nice-to-have, not core functionality.
  }
}

// Reads region + realm names out of the game's own files for the current
// path and prefills the form. With `autofill` the first (most recently
// played) realm is resolved to its slug automatically; otherwise the realm
// dropdown just becomes available for the user to pick from.
async function refreshGameSettings({ autofill } = { autofill: false }) {
  const path = wowPathEl.value.trim();
  gameRealmsLabel.hidden = true;
  if (!path) return;

  let settings;
  try {
    settings = await invoke("detect_game", { wowRetailPath: path });
  } catch {
    return; // unreadable path — manual entry still works
  }

  if (settings.region === "eu" || settings.region === "us") {
    regionEl.value = settings.region;
  }

  if (settings.realmNames.length > 0) {
    gameRealmsEl.replaceChildren(new Option("— pick a realm —", ""));
    for (const name of settings.realmNames) {
      gameRealmsEl.add(new Option(name, name));
    }
    gameRealmsLabel.hidden = false;

    if (autofill) {
      gameRealmsEl.value = settings.realmNames[0];
      await resolveSelectedRealm();
    }
  }
}

async function resolveSelectedRealm() {
  const name = gameRealmsEl.value;
  if (!name) return;
  try {
    const resolved = await invoke("resolve_realm", { region: regionEl.value, name });
    realmSlugEl.value = resolved.slug;
    showFeedback(`${name} → ${resolved.slug}`, false);
  } catch (e) {
    showFeedback(String(e), true);
  }
}

detectBtn.addEventListener("click", async () => {
  try {
    const detected = await invoke("detect_wow_path");
    if (detected) {
      wowPathEl.value = detected;
      showFeedback("Detected a WoW retail install.", false);
      await refreshGameSettings({ autofill: !realmSlugEl.value.trim() });
    } else {
      showFeedback("Couldn't auto-detect a WoW retail install — use Browse… to point at it.", true);
    }
  } catch (e) {
    showFeedback(`Detect failed: ${e}`, true);
  }
});

browseBtn.addEventListener("click", async () => {
  try {
    const picked = await invoke("pick_wow_path");
    if (picked) {
      wowPathEl.value = picked;
      showFeedback("Path set.", false);
      await refreshGameSettings({ autofill: !realmSlugEl.value.trim() });
    }
  } catch (e) {
    showFeedback(String(e), true);
  }
});

gameRealmsEl.addEventListener("change", resolveSelectedRealm);

wowPathEl.addEventListener("change", () => refreshGameSettings({ autofill: false }));

syncNowBtn.addEventListener("click", async () => {
  try {
    await invoke("sync_now");
    showFeedback("Sync requested.", false);
    setTimeout(refreshStatus, 1500);
  } catch (e) {
    showFeedback(`Sync failed: ${e}`, true);
  }
});

form.addEventListener("submit", async (event) => {
  event.preventDefault();

  const intervalMinutes = Number.parseInt(intervalEl.value, 10);
  if (!Number.isFinite(intervalMinutes) || intervalMinutes < 1) {
    showFeedback("Sync interval must be at least 1 minute.", true);
    return;
  }
  if (!realmSlugEl.value.trim()) {
    showFeedback("Realm slug is required.", true);
    return;
  }

  const config = {
    region: regionEl.value,
    realmSlug: realmSlugEl.value.trim(),
    wowRetailPath: wowPathEl.value.trim(),
    intervalMinutes,
    launchAtStartup: launchAtStartupEl.checked,
  };

  try {
    await invoke("save_config", { config });
    showFeedback("Saved — sync applies immediately.", false);
  } catch (e) {
    showFeedback(`Save failed: ${e}`, true);
  }
});

loadConfig();
refreshStatus();
setInterval(refreshStatus, 15_000);
