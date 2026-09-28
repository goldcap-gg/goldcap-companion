import { relativeTime, countdown, groupDigits, foreverScanLine, foreverCrowdLine, foreverMarketLine } from "../lib/format.js";
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

// Enabled Forever installs only — a game the player turned off must read as absent here too,
// even while stale state from before they turned it off still sits in forever.json on disk.
function foreverGames(s) {
  return s.foreverEnabled ? (s.games ?? []).filter((g) => g.game === "forever") : [];
}

function foreverBroken(games) {
  return games.some((g) => g.unauthorized);
}

// The earliest failing link, in pipeline order — the ones after it may only
// be failing because of it, so it is the one worth naming. Retail's three
// stages only exist to consult once Retail is actually on; a Forever-only
// player is judged by its own installs instead.
function heroPhrase(s) {
  if (!s.configured) return "Not configured";
  if (s.retailEnabled) {
    const broken = ORDER.find((k) => s[k].state === "broken");
    if (broken) return BROKEN_PHRASE[broken];
    const idle = ORDER.find((k) => s[k].state === "notConnected");
    if (idle) return IDLE_PHRASE[idle];
  }
  const games = foreverGames(s);
  if (foreverBroken(games)) return "WoW: Forever needs attention";
  if (s.retailEnabled) return "Everything works";
  return games.some((g) => g.lastSentAt) ? "Everything works" : "Waiting for your first scan";
}

function heroState(s) {
  const games = foreverGames(s);
  if (foreverBroken(games)) return "broken";
  if (!s.retailEnabled) return games.some((g) => g.lastSentAt) ? "ok" : "notConnected";
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

// One Forever install's card: market (realm · faction), the scan and crowd-prices lines, any
// note the site sent back, and the fixed reminder about when a scan actually reaches disk.
// Returns the card alongside its two age-bearing paragraphs so paintTime can re-date them every
// second without rebuilding the card (and losing focus/animation the way paintSnapshot's own
// rebuild would).
function foreverCard(g) {
  const card = document.createElement("div");
  card.className = "card";

  const head = document.createElement("div");
  head.className = "row";
  const title = document.createElement("span");
  title.className = "stage-title";
  title.textContent = "WoW: Forever";
  head.append(title);
  const market = foreverMarketLine(g);
  if (market) {
    const where = document.createElement("span");
    where.className = "muted mono";
    where.textContent = market;
    head.append(where);
  }
  card.append(head);

  const scan = document.createElement("p");
  scan.className = "stage-detail";
  card.append(scan);

  const crowd = document.createElement("p");
  crowd.className = "stage-detail";
  card.append(crowd);

  if (g.note) {
    const note = document.createElement("p");
    note.className = "stage-error mono";
    note.textContent = g.note;
    card.append(note);
  }

  const hint = document.createElement("p");
  hint.className = "muted";
  hint.textContent =
    "A new scan uploads after /reload or logging out — that is when WoW writes it to disk.";
  card.append(hint);

  return { card, scanEl: scan, crowdEl: crowd };
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

  // The Retail card: a small title row above the same three stage rows this screen has always
  // shown, wrapped in .card so it reads next to the Forever card(s) rather than floating loose —
  // hidden outright while Retail is off (see paintSnapshot).
  const retailCard = document.createElement("div");
  retailCard.className = "card";
  const retailHead = document.createElement("div");
  retailHead.className = "row";
  const retailTitle = document.createElement("span");
  retailTitle.className = "stage-title";
  retailTitle.textContent = "Retail";
  const retailWhere = document.createElement("span");
  retailWhere.className = "muted mono";
  retailHead.append(retailTitle, retailWhere);
  const stages = document.createElement("div");
  stages.className = "stages";
  retailCard.append(retailHead, stages);

  // One card per Forever install currently enabled — rebuilt on every structural repaint
  // (installs can appear/disappear between polls), unlike the Retail card above which is
  // built once and only ever hidden/shown.
  const foreverCards = document.createElement("div");
  foreverCards.className = "stack";

  const meta = document.createElement("p");
  meta.className = "meta dim";

  const action = document.createElement("button");
  action.className = "btn btn-primary";
  action.type = "button";

  const foot = document.createElement("p");
  foot.className = "foot dim";

  // Everything between the topbar and the pinned Sync button/version line.
  // A plain .spacer (flex:1, no overflow handling) used to sit where this
  // wrapper's gap now goes — fine as long as hero+stages+meta always fit
  // the fixed window height, but the update bar (or several stage rows
  // carrying real error text at once) can push their combined height past
  // what's left, and a .spacer doesn't shrink: it just lets its neighbors
  // overflow the screen and get clipped by body's `overflow: hidden`. This
  // wrapper absorbs the same leftover space when there's slack, but also
  // shrinks and scrolls internally when there isn't, so the Sync button and
  // version line stay fully visible either way.
  const scroll = document.createElement("div");
  scroll.className = "status-scroll";
  scroll.append(hero, foreverCards, retailCard, meta);

  el.append(top, scroll, action, foot);

  let snapshot = null;
  let disposed = false;
  // One entry per stage row, so the 1-second timer can rewrite just the
  // detail text instead of rebuilding the row (and evicting focus from it).
  let stageDetails = [];
  // One entry per Forever card, same reason: the 1-second timer re-dates the scan/crowd lines
  // without rebuilding the card underneath a focused element.
  let foreverDetails = [];

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
      ? snapshot.retailEnabled
        ? `${snapshot.realmSlug} · ${snapshot.region.toUpperCase()}`
        : "WoW: Forever"
      : "not set up yet";
    const headline = document.createElement("div");
    headline.className = "row";
    headline.append(dot, phrase);
    hero.append(headline, where);

    // Retail's three stage rows only exist while Retail is actually on — a disabled game gets
    // no card at all, never a card full of "Retail is off" rows.
    retailCard.hidden = !snapshot.retailEnabled;
    if (snapshot.retailEnabled) {
      retailWhere.textContent = `${snapshot.realmSlug} · ${snapshot.region.toUpperCase()}`;
      // Pairing is only offered once there is a realm to pair against — an
      // unconfigured companion routes to the wizard, not to Settings.
      const canPair = snapshot.configured && !snapshot.paired;
      const rows = ORDER.map((k) => {
        const { row, detailEl } = stageRow(k, snapshot[k], ctx, canPair);
        return { row, stage: snapshot[k], el: detailEl };
      });
      stageDetails = rows.map(({ stage, el }, i) => ({ key: ORDER[i], stage, el }));
      stages.replaceChildren(...rows.map(({ row }) => row));
    } else {
      stageDetails = [];
    }

    const games = foreverGames(snapshot);
    const cards = games.map((g) => ({ g, ...foreverCard(g) }));
    foreverDetails = cards;
    foreverCards.replaceChildren(...cards.map(({ card }) => card));

    action.textContent = snapshot.syncing ? "Syncing…" : "Sync now";
    action.disabled = snapshot.syncing || !snapshot.configured;

    foot.textContent = `v${snapshot.version}`;

    paintTime();
  }

  // Only the time-derived strings: each stage's relative age, the Forever cards' scan/crowd
  // lines, and the countdown. Runs every second, independent of the poll, so times keep moving
  // between snapshots without touching the DOM nodes above them.
  function paintTime() {
    if (!snapshot) return;
    const now = Math.floor(Date.now() / 1000);

    for (const { key, stage, el } of stageDetails) {
      const age = relativeTime(stage.at, now);
      let text = age ? `${stage.detail} · ${age}` : stage.detail;
      // The whole-market payload rides the prices leg: how many items it carried and how
      // old its snapshot is, re-dated every second like the stage's own age.
      if (key === "prices" && snapshot.marketItems) {
        text += ` · ${groupDigits(snapshot.marketItems)} market items, priced ${relativeTime(snapshot.marketTs, now)}`;
      }
      el.textContent = text;
    }

    for (const { g, scanEl, crowdEl } of foreverDetails) {
      scanEl.textContent = foreverScanLine(g, now);
      const crowdText = foreverCrowdLine(g, now);
      crowdEl.textContent = crowdText;
      crowdEl.hidden = !crowdText;
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
