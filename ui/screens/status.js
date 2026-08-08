import { relativeTime, countdown } from "../lib/format.js";

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

// The hero phrase is the first thing that is wrong, in pipeline order — the
// earliest broken link is the one worth fixing first.
function heroPhrase(s) {
  if (!s.configured) return "Not configured";
  if (s.prices.state === "broken") return "Prices are not updating";
  if (s.addon.state === "broken") return "The addon is not receiving prices";
  if (s.ledger.state === "broken") return "Ledger is not uploading";
  if (s.ledger.state === "notConnected") return "Prices are flowing";
  if (s.prices.state === "notConnected") return "Waiting for the first sync";
  return "Everything works";
}

function heroState(s) {
  const stages = [s.prices, s.addon, s.ledger];
  if (stages.some((st) => st.state === "broken")) return "broken";
  if (stages.every((st) => st.state === "ok")) return "ok";
  return "notConnected";
}

function stageRow(key, stage, now, ctx) {
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
  const age = relativeTime(stage.at, now);
  detail.textContent = age ? `${stage.detail} · ${age}` : stage.detail;
  body.append(detail);

  if (stage.error) {
    const err = document.createElement("div");
    err.className = "stage-error mono";
    err.textContent = stage.error;
    body.append(err);
  }

  // Only the ledger has an action the user can take from here.
  if (key === "ledger" && stage.state === "notConnected" && stage.detail.includes("Not paired")) {
    const pair = document.createElement("button");
    pair.className = "btn btn-primary btn-sm";
    pair.textContent = "Pair";
    pair.addEventListener("click", () => ctx.show("settings"));
    body.append(pair);
  }

  row.append(body);
  return row;
}

export function render(el, ctx) {
  el.classList.add("screen-status");

  const top = document.createElement("div");
  top.className = "topbar";
  top.innerHTML = `<div class="brand"><span class="brand-mark"></span>GoldCap</div>`;
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

  function paint() {
    if (!snapshot) return;
    const now = Math.floor(Date.now() / 1000);

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

    stages.replaceChildren(
      ...["prices", "addon", "ledger"].map((k) => stageRow(k, snapshot[k], now, ctx)),
    );

    const left = countdown(snapshot.nextTickAt, now);
    meta.textContent = snapshot.syncing
      ? "Syncing now…"
      : left
        ? `Next sync in ${left}`
        : "";

    action.textContent = snapshot.syncing ? "Syncing…" : "Sync now";
    action.disabled = snapshot.syncing || !snapshot.configured;

    foot.textContent = `v${snapshot.version}`;
  }

  async function refresh() {
    try {
      snapshot = await ctx.api.getStatus();
      paint();
    } catch (e) {
      ctx.toast(String(e), true);
    }
  }

  action.addEventListener("click", async () => {
    action.disabled = true;
    try {
      await ctx.api.syncNow();
      // Paint the in-flight state immediately rather than waiting up to a
      // full poll for the backend to admit it started.
      if (snapshot) {
        snapshot = { ...snapshot, syncing: true };
        paint();
      }
      setTimeout(refresh, 1200);
    } catch (e) {
      ctx.toast(String(e), true);
      action.disabled = false;
    }
  });

  refresh();
  const poll = setInterval(refresh, POLL_MS);
  // Independent of the poll so relative times and the countdown keep moving
  // even while a request is in flight.
  const tick = setInterval(paint, 1000);

  return {
    dispose() {
      clearInterval(poll);
      clearInterval(tick);
    },
  };
}
