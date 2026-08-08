import { render as renderWizard } from "./screens/wizard.js";
import { render as renderStatus } from "./screens/status.js";
import { render as renderSettings } from "./screens/settings.js";

const SCREENS = {
  wizard: renderWizard,
  status: renderStatus,
  settings: renderSettings,
};

let ctx = null;
let current = null;

export function toast(message, isError = false) {
  const el = document.getElementById("toast");
  el.textContent = message;
  el.classList.toggle("error", Boolean(isError));
  el.classList.add("show");
  clearTimeout(el._timer);
  el._timer = setTimeout(() => el.classList.remove("show"), 2000);
}

/// Swaps the visible screen. Screens own their own polling and must stop it
/// on teardown, which is what the returned disposer is for.
export function show(screen) {
  if (current?.dispose) current.dispose();
  // Cleared before the swap: if the render below throws, the next show()
  // must not dispose a screen that is already gone.
  current = null;

  const root = ctx.root;
  root.replaceChildren();
  const section = document.createElement("section");
  section.className = `screen screen-${screen}`;
  root.append(section);

  try {
    current = SCREENS[screen](section, ctx) ?? {};
  } catch (e) {
    // A half-built screen is worse than none: replace it with something
    // that says what happened, so the window is never silently dead.
    current = {};
    section.replaceChildren();
    const failure = document.createElement("p");
    failure.className = "muted";
    failure.textContent = `Could not open this screen: ${e}`;
    section.append(failure);
    toast(String(e), true);
  }

  // Re-trigger the enter transition on every swap.
  requestAnimationFrame(() => section.classList.add("screen-in"));
}

export async function mount(root, api) {
  ctx = { root, api, show, toast };
  try {
    const config = await api.getConfig();
    const complete =
      config.realmSlug.trim() !== "" && config.wowRetailPath.trim() !== "";
    show(complete ? "status" : "wizard");
  } catch (e) {
    root.textContent = `Could not read settings: ${e}`;
  }
}
