import { relativeTime, countdown } from "../lib/format.js";
import { brandMarkSvg } from "../lib/brandMark.js";

const POLL_MS = 5000;

const DOT_CLASS = {
  ok: "dot dot-ok",
  notConnected: "dot dot-idle",
  broken: "dot dot-broken",
};

const STAGE_TITLES = {
  prices: "Prices from goldcap.gg",
  addon: "Written to the addon",
  ledger: "Ledger to the site",
};

const ORDER = ["prices", "addon", "ledger"];

const BROKEN_PHRASE = {
  prices: "Prices are not updating",
  addon: "The addon is not receiving prices",
  ledger: "Ledger is not uploading",
};

// Not a failure: the pipeline simply does not reach this far yet.
const IDLE_PHRASE = {
  prices: "Waiting for the first sync",
  addon: "No prices written yet",
  ledger: "Prices are flowing",
};

// The earliest failing link, in pipeline order — the ones after it may only
// be failing because of it, so it is the one worth naming.
function heroPhrase(s) {
  if (!s.configured) return "Not configured";
  const broken = ORDER.find((k) => s[k].state === "broken");
  if (broken) return BROKEN_PHRASE[broken];
  const idle = ORDER.find((k) => s[k].state === "notConnected");
  if (idle) return IDLE_PHRASE[idle];
  return "Everything works";
}

function heroState(s) {
  const stages = [s.prices, s.addon, s.ledger];
  if (stages.some((st) => st.state === "broken")) return "broken";
  if (stages.every((st) => st.state === "ok")) return "ok";
  return "notConnected";
}

// Structural: title, dot, error line and the Pair button. Returns the
// detail element alongside the row so the 1-second timer can rewrite just
// that text without tearing the row (and any focus inside it) down.
function stageRow(key, stage, ctx, canPair) {
  const row = document.createElement("div");
  row.className = "stage";

  const dot = document.createElement("span");
  dot.className = DOT_CLASS[stage.state];
  row.append(dot);

  const body = document.createElement("div");
  body.className = "stage-body";

  const title = document.createElement("div");
  title.className = "stage-title";
  title.textContent = STAGE_TITLES[key];
  body.append(title);

  const detail = document.createElement("div");
  detail.className = "stage-detail";
  detail.textContent = stage.detail;
  body.append(detail);

  if (stage.error) {
    const err = document.createElement("div");
    err.className = "stage-error mono";
    err.textContent = stage.error;
    body.append(err);
  }

  // Only the ledger has an action the user can take from here. Gated on the
  // snapshot's own `paired` flag rather than sniffing the detail string, so
  // a reworded Rust message cannot make this button silently vanish.
  if (key === "ledger" && canPair && stage.state === "notConnected") {
    const pair = document.createElement("button");
    pair.className = "btn btn-primary btn-sm";
    pair.textContent = "Pair";
    pair.addEventListener("click", () => ctx.show("settings"));
    body.append(pair);
  }

  row.append(body);
  return { row, detailEl: detail };
}

export function render(el, ctx) {
  el.classList.add("screen-status");

  const top = document.createElement("div");
  top.className = "topbar";
  top.innerHTML = `<div class="brand">${brandMarkSvg()}GoldCap</div>`;
  const gear = document.createElement("button");
  gear.className = "icon-btn";
  gear.type = "button";
  gear.title = "Settings";
  gear.setAttribute("aria-label", "Settings");
  gear.textContent = "⚙";
  gear.addEventListener("click", () => ctx.show("settings"));
  top.append(gear);

  const hero = document.createElement("div");
  hero.className = "card card-hero hero";

  const stages = document.createElement("div");
  stages.className = "stages";

  const meta = document.createElement("p");
  meta.className = "meta dim";

  const action = document.createElement("button");
  action.className = "btn btn-primary";
  action.type = "button";

  const foot = document.createElement("p");
  foot.className = "foot dim";

  const spacer = document.createElement("div");
  spacer.className = "spacer";

  el.append(top, hero, stages, meta, spacer, action, foot);

  let snapshot = null;
  let disposed = false;
  // One entry per stage row, so the 1-second timer can rewrite just the
  // detail text instead of rebuilding the row (and evicting focus from it).
  let stageDetails = [];

  // Everything structural: hero, stage rows (titles, dots, errors, the
  // Pair button), the Sync button's label/disabled state, the version
  // footer. Runs only when a new snapshot lands — never on the 1-second
  // timer — so a focused Pair button and the hero dot's animation both
  // survive between polls.
  function paintSnapshot() {
    if (!snapshot) return;

    hero.replaceChildren();
    const dot = document.createElement("span");
    dot.className = `${DOT_CLASS[heroState(snapshot)]} dot-lg${snapshot.syncing ? " dot-pulse" : ""}`;
    const phrase = document.createElement("h1");
    phrase.textContent = heroPhrase(snapshot);
    const where = document.createElement("p");
    where.className = "muted hero-where mono";
    where.textContent = snapshot.configured
      ? `${snapshot.realmSlug} · ${snapshot.region.toUpperCase()}`
      : "no realm yet";
    const headline = document.createElement("div");
    headline.className = "row";
    headline.append(dot, phrase);
    hero.append(headline, where);

    // Pairing is only offered once there is a realm to pair against — an
    // unconfigured companion routes to the wizard, not to Settings.
    const canPair = snapshot.configured && !snapshot.paired;
    const rows = ORDER.map((k) => {
      const { row, detailEl } = stageRow(k, snapshot[k], ctx, canPair);
      return { row, stage: snapshot[k], el: detailEl };
    });
    stageDetails = rows.map(({ stage, el }) => ({ stage, el }));
    stages.replaceChildren(...rows.map(({ row }) => row));

    action.textContent = snapshot.syncing ? "Syncing…" : "Sync now";
    action.disabled = snapshot.syncing || !snapshot.configured;

    foot.textContent = `v${snapshot.version}`;

    paintTime();
  }

  // Only the time-derived strings: each stage's relative age and the
  // countdown. Runs every second, independent of the poll, so times keep
  // moving between snapshots without touching the DOM nodes above them.
  function paintTime() {
    if (!snapshot) return;
    const now = Math.floor(Date.now() / 1000);

    for (const { stage, el } of stageDetails) {
      const age = relativeTime(stage.at, now);
      el.textContent = age ? `${stage.detail} · ${age}` : stage.detail;
    }

    const left = countdown(snapshot.nextTickAt, now);
    meta.textContent = snapshot.syncing
      ? "Syncing now…"
      : left
        ? `Next sync in ${left}`
        : "";
  }

  async function refresh() {
    try {
      const s = await ctx.api.getStatus();
      if (disposed) return;
      // Most polls land between ticks and report the same thing the last
      // one did. Rebuilding the stage rows then would evict focus from the
      // Pair button (or restart the hero dot's pulse) for no reason, so
      // only run the structural repaint when something actually changed.
      const unchanged = snapshot && JSON.stringify(s) === JSON.stringify(snapshot);
      snapshot = s;
      if (unchanged) {
        paintTime();
      } else {
        paintSnapshot();
      }
    } catch (e) {
      if (disposed) return;
      ctx.toast(String(e), true);
    }
  }

  action.addEventListener("click", async () => {
    action.disabled = true;
    try {
      await ctx.api.syncNow();
      if (disposed) return;
      // Paint the in-flight state immediately rather than waiting up to a
      // full poll for the backend to admit it started.
      if (snapshot) {
        snapshot = { ...snapshot, syncing: true };
        paintSnapshot();
      }
      setTimeout(refresh, 1200);
    } catch (e) {
      if (disposed) return;
      ctx.toast(String(e), true);
      action.disabled = false;
    }
  });

  refresh();
  const poll = setInterval(refresh, POLL_MS);
  // Independent of the poll so relative times and the countdown keep moving
  // even while a request is in flight.
  const tick = setInterval(paintTime, 1000);

  return {
    dispose() {
      disposed = true;
      clearInterval(poll);
      clearInterval(tick);
    },
  };
}
