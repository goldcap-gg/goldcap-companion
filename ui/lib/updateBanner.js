import { installUpdate, updateReady } from "./api.js";

/**
 * The "an update is waiting" bar.
 *
 * The updater has always found and staged updates on its own — but the only
 * place it said so was a tray menu item, and nobody opens the menu of a tray
 * app that appears to be working. A companion left running for weeks could
 * therefore sit on an old build with nothing on screen ever mentioning it.
 * This is the visible half: whenever the window is open, the offer is on it.
 *
 * Mounted once by app.js above the screens, so it survives screen swaps and
 * every screen inherits it. It appears on two occasions — the check that
 * lands while the window is open (the `update-staged` event) and a window
 * opened after the fact (the `update_ready` query on mount).
 */
export function mountUpdateBanner(host) {
  const bar = document.createElement("div");
  bar.className = "update-bar";
  bar.hidden = true;

  const text = document.createElement("p");
  text.className = "update-bar-text";

  const action = document.createElement("button");
  action.type = "button";
  action.className = "update-bar-action";

  bar.append(text, action);
  host.prepend(bar);

  let current = null;

  function render(update) {
    if (!update) {
      bar.hidden = true;
      return;
    }
    current = update;
    text.textContent =
      update.kind === "manual"
        ? `Version ${update.version} is out, but this copy can't update itself.`
        : `Version ${update.version} is ready to install.`;
    action.textContent = update.action;
    bar.hidden = false;
  }

  action.addEventListener("click", async () => {
    if (!current) return;
    // "Restart now" ends this process, so there is no success path to render
    // — only a failure worth reporting. The tray item and this bar both
    // survive a failed attempt, so nothing is lost either way.
    action.disabled = true;
    try {
      await installUpdate();
    } finally {
      action.disabled = false;
    }
  });

  updateReady()
    .then(render)
    // A window that can't ask about updates should still be a usable window.
    .catch(() => {});

  const events = window.__TAURI__?.event;
  if (events) events.listen("update-staged", (e) => render(e.payload));

  return { render };
}
