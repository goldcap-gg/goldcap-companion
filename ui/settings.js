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
const syncNowBtn = document.getElementById("sync-now-btn");

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

detectBtn.addEventListener("click", async () => {
  try {
    const detected = await invoke("detect_wow_path");
    if (detected) {
      wowPathEl.value = detected;
      showFeedback("Detected a WoW retail install.", false);
    } else {
      showFeedback("Couldn't auto-detect a WoW retail install — enter the path manually.", true);
    }
  } catch (e) {
    showFeedback(`Detect failed: ${e}`, true);
  }
});

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
