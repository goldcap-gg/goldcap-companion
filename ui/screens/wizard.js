import { truncateMiddle, formatPairCode } from "../lib/format.js";

// The install path and, when the game folders resolve to exactly one, the
// realm all come out of the game's own files — see detect() below. Step 0
// (Games) is where a player says which games GoldCap should keep in sync;
// step 1 (Setup) only ever shows the pieces the games actually turned on
// need (a realm picker for Retail, nothing at all for Forever); step 2
// (Connect) is the pairing screen, same as before this feature existed.
const STEPS = ["Games", "Setup", "Connect"];

export function render(el, ctx) {
  el.classList.add("screen-wizard");

  // Fallback shape only — matches Config::default() on the Rust side. The
  // very first thing detect() does is overwrite every one of these fields
  // from the actually-saved config (see below), so this is only what a
  // config load that outright fails (not "empty", an IPC error) leaves
  // behind. Nothing here should be read as "the default a fresh wizard
  // writes" — a fresh wizard writes whatever was already on disk.
  const draft = {
    region: "eu",
    realmSlug: "",
    wowRetailPath: "",
    wowRootPath: "",
    retailEnabled: false,
    foreverEnabled: false,
    intervalMinutes: 30,
    launchAtStartup: false,
    companionToken: "",
  };
  let detectedGames = []; // DetectedGame[] from api.detectGames(wowRootPath)
  let realmNames = [];
  let step = 0;

  const progress = document.createElement("div");
  progress.className = "progress";
  for (const name of STEPS) {
    const seg = document.createElement("span");
    seg.className = "progress-seg";
    seg.title = name;
    progress.append(seg);
  }

  const head = document.createElement("div");
  head.className = "wizard-head";

  const bodyEl = document.createElement("div");
  bodyEl.className = "wizard-body stack";

  const nav = document.createElement("div");
  nav.className = "wizard-nav row";

  el.append(
    progress,
    head,
    bodyEl,
    Object.assign(document.createElement("div"), { className: "spacer" }),
    nav,
  );

  function paintProgress() {
    [...progress.children].forEach((seg, i) => {
      seg.classList.toggle("done", i < step);
      seg.classList.toggle("current", i === step);
    });
  }

  // Set by setHead on every step, then focused by go() once the step has
  // rendered — the standard wizard pattern: the keyboard cursor and a
  // screen reader's attention both land on the new step's heading instead
  // of falling through to <body> when nav.replaceChildren() below removes
  // whatever button the user just activated.
  let headingEl = null;

  function setHead(title, sub) {
    head.replaceChildren();
    const h = document.createElement("h1");
    h.textContent = title;
    h.tabIndex = -1;
    const p = document.createElement("p");
    p.className = "muted wizard-sub";
    p.textContent = sub;
    head.append(h, p);
    headingEl = h;
  }

  // Bumped on every navigation, including detect()'s own — guards against a
  // detect() still in flight (auto-detect, detectGames) landing after the
  // user has already moved on some other way.
  let requestGen = 0;

  function go(next) {
    requestGen++;
    step = next;
    bodyEl.classList.remove("step-in");
    render_step();
    paintProgress();
    // render_step() runs each stepX() function synchronously up to its
    // first await, and setHead() is always the first thing each one does —
    // so by this point the new heading already exists in the live DOM.
    headingEl?.focus();
    requestAnimationFrame(() => bodyEl.classList.add("step-in"));
  }

  // ---- detecting: runs once on entry, before any step exists -------------
  //
  // Everything the Games step needs to offer a choice — the WoW root and
  // which games live under it — is read straight out of the filesystem.
  // Silent throughout: a failed probe here just means the step still exists
  // to ask the question by hand (an empty root, an empty games list).
  async function detect() {
    const gen = ++requestGen;

    let config = null;
    try {
      config = await ctx.api.getConfig();
    } catch {
      // Fall through with the fallback draft above, same as an empty saved config.
    }
    if (gen !== requestGen) return;

    // Seed everything the wizard never asks about directly — interval,
    // launch-at-startup, any pairing token already on disk — from what is
    // actually saved, before anything below touches wowRootPath/the game
    // toggles/region/realmSlug. Without this, a config from a previous
    // visit that this pass reopens for (say, Retail chosen but no realm
    // resolved yet before the player hit "Later") would have every one of
    // these silently reset the moment it saves.
    if (config) {
      draft.intervalMinutes = config.intervalMinutes;
      draft.launchAtStartup = config.launchAtStartup;
      draft.companionToken = config.companionToken;
      draft.region = config.region;
      draft.realmSlug = config.realmSlug;
      draft.wowRetailPath = config.wowRetailPath;
      draft.wowRootPath = config.wowRootPath;
      draft.retailEnabled = config.retailEnabled;
      draft.foreverEnabled = config.foreverEnabled;
    }

    if (!draft.wowRootPath) {
      try {
        draft.wowRootPath = (await ctx.api.detectWowRoot()) || "";
      } catch {
        draft.wowRootPath = "";
      }
      if (gen !== requestGen) return;
    }

    await loadDetectedGames(gen);
    if (gen !== requestGen) return;

    go(0);
  }

  // Re-reads which games exist under draft.wowRootPath. A config that never
  // decided either game (a brand-new install, or an old config migrated with
  // neither turned on) gets the checkboxes defaulted to whatever was
  // actually found — matching the "found" badges the step shows. A config
  // that already decided (a returning "Later" visit) keeps its own choice
  // even if a game folder has since disappeared, so a player is never
  // silently un-enrolled by an install being temporarily unavailable.
  async function loadDetectedGames(gen) {
    if (!draft.wowRootPath) {
      detectedGames = [];
      return;
    }
    try {
      detectedGames = await ctx.api.detectGames(draft.wowRootPath);
    } catch {
      detectedGames = [];
    }
    if (gen !== undefined && gen !== requestGen) return;
    if (!draft.retailEnabled && !draft.foreverEnabled) {
      draft.retailEnabled = detectedGames.some((g) => g.kind === "retail");
      draft.foreverEnabled = detectedGames.some((g) => g.kind === "forever");
    }
  }

  // A checkbox row for one found game: title, a short line about what it
  // does, and a "found" badge — mirrors the design canvas's Games step.
  function gameCheckboxRow(title, subtitle, checked, onChange) {
    const label = document.createElement("label");
    label.className = "checkbox-row card";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = checked;
    box.addEventListener("change", () => onChange(box.checked));
    const body = document.createElement("div");
    body.className = "stage-body";
    const t = document.createElement("span");
    t.className = "stage-title";
    t.textContent = title;
    const sub = document.createElement("span");
    sub.className = "muted";
    sub.textContent = subtitle;
    body.append(t, sub);
    label.append(box, body);
    return label;
  }

  // Classic Era: shown, not omitted, specifically disabled so a player who
  // has it installed knows GoldCap saw it and left it alone on purpose.
  function unsupportedGameRow(title, subtitle) {
    const row = document.createElement("div");
    row.className = "checkbox-row card muted";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.disabled = true;
    box.setAttribute("aria-label", title);
    const body = document.createElement("div");
    body.className = "stage-body";
    const t = document.createElement("span");
    t.textContent = title;
    const sub = document.createElement("span");
    sub.className = "muted";
    sub.textContent = subtitle;
    body.append(t, sub);
    row.append(box, body);
    return row;
  }

  // ---- step 0: which games ------------------------------------------------

  function stepGames() {
    setHead("Which WoW do you play?", "We found your World of Warcraft folder. Pick the games GoldCap should keep in sync.");
    bodyEl.replaceChildren();

    const pathRow = document.createElement("div");
    pathRow.className = "card row";
    const pathLabel = document.createElement("span");
    pathLabel.className = "mono";
    const rowSpacer = document.createElement("span");
    rowSpacer.className = "spacer";
    const change = document.createElement("button");
    change.className = "btn btn-sm";
    change.type = "button";
    change.textContent = "Change…";
    pathRow.append(pathLabel, rowSpacer, change);

    const list = document.createElement("div");
    list.className = "stack";

    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "No install found — point at your World of Warcraft folder.";

    const hint = document.createElement("p");
    hint.className = "muted wizard-hint";
    hint.textContent = "Only WoW: Forever? Leave Retail off — nothing else to set up.";

    const next = document.createElement("button");
    next.className = "btn btn-primary";
    next.type = "button";
    next.textContent = "Next →";

    function paintPath() {
      pathLabel.textContent = draft.wowRootPath ? truncateMiddle(draft.wowRootPath, 42) : "No folder set";
      pathLabel.title = draft.wowRootPath;
    }

    function paintNext() {
      next.disabled = !draft.wowRootPath || (!draft.retailEnabled && !draft.foreverEnabled);
    }

    function paintGames() {
      list.replaceChildren();
      const retail = detectedGames.find((g) => g.kind === "retail");
      const forever = detectedGames.find((g) => g.kind === "forever");
      const classicEra = detectedGames.find((g) => g.kind === "classicEra");

      if (forever) {
        list.append(
          gameCheckboxRow(
            "WoW: Forever",
            "Client found · uploads your scans, brings back crowd prices",
            draft.foreverEnabled,
            (checked) => {
              draft.foreverEnabled = checked;
              paintNext();
            },
          ),
        );
      }
      if (retail) {
        list.append(
          gameCheckboxRow(
            "Retail",
            "Client found · realm prices, ledger",
            draft.retailEnabled,
            (checked) => {
              draft.retailEnabled = checked;
              paintNext();
            },
          ),
        );
      }
      if (classicEra) {
        list.append(unsupportedGameRow("Classic Era", "Found, not supported by GoldCap"));
      }

      empty.hidden = detectedGames.length > 0;
      list.hidden = detectedGames.length === 0;
      paintNext();
    }

    change.addEventListener("click", async () => {
      try {
        const picked = await ctx.api.pickWowRoot();
        if (!picked) return;
        draft.wowRootPath = picked;
        paintPath();
        await loadDetectedGames();
        paintGames();
      } catch (e) {
        ctx.toast(String(e), true);
      }
    });

    next.addEventListener("click", () => go(1));

    bodyEl.append(pathRow, empty, list, hint);
    nav.replaceChildren(Object.assign(document.createElement("div"), { className: "spacer" }), next);

    paintPath();
    paintGames();
  }

  // ---- step 1: your games, set up -----------------------------------------

  function stepSetup() {
    setHead("Your games, set up", "");
    bodyEl.replaceChildren();

    const back = document.createElement("button");
    back.className = "btn btn-ghost";
    back.type = "button";
    back.textContent = "← Back";
    back.addEventListener("click", () => go(0));

    const next = document.createElement("button");
    next.className = "btn btn-primary";
    next.type = "button";
    next.textContent = "Connect to goldcap.gg →";

    function paintNext() {
      next.disabled = draft.retailEnabled && draft.realmSlug.trim() === "";
    }
    next.disabled = true;

    if (draft.retailEnabled) {
      const card = document.createElement("div");
      card.className = "card stack";

      const head = document.createElement("div");
      head.className = "row";
      const title = document.createElement("span");
      title.className = "stage-title";
      title.textContent = "Retail";
      const headSpacer = document.createElement("span");
      headSpacer.className = "spacer";
      const per = document.createElement("span");
      per.className = "muted";
      per.textContent = "prices come per realm";
      head.append(title, headSpacer, per);

      const regionLabel = document.createElement("label");
      regionLabel.className = "label";
      regionLabel.textContent = "Region";
      regionLabel.htmlFor = "wizard-region";
      const region = document.createElement("select");
      region.className = "field";
      region.id = "wizard-region";
      region.add(new Option("EU", "eu"));
      region.add(new Option("US", "us"));
      region.add(new Option("KR", "kr"));
      region.add(new Option("TW", "tw"));
      region.value = draft.region;

      const realmLabel = document.createElement("label");
      realmLabel.className = "label";
      realmLabel.textContent = "Realm";
      realmLabel.htmlFor = "wizard-realm";
      const realm = document.createElement("select");
      realm.className = "field";
      realm.id = "wizard-realm";

      const resolved = document.createElement("p");
      resolved.className = "mono resolved";

      const manual = document.createElement("input");
      manual.id = "wizard-realm-slug";
      manual.className = "field mono";
      manual.placeholder = "realm slug, e.g. dentarg";
      manual.setAttribute("aria-label", "Realm slug");
      manual.hidden = true;

      const manualToggle = document.createElement("button");
      manualToggle.className = "link";
      manualToggle.type = "button";
      manualToggle.textContent = "enter the slug manually";

      function paintResolved() {
        const ok = draft.realmSlug.trim() !== "";
        resolved.textContent = ok ? `→ ${draft.realmSlug}` : "";
        paintNext();
      }

      // Bumped by anything that supersedes an in-flight lookup — the same
      // discipline the old realm step used, so a slow resolve can never
      // land after the user has picked something else and overwrite it.
      let realmRequest = 0;

      async function resolveSelected() {
        const name = realm.value;
        if (!name) return;
        const request = ++realmRequest;
        try {
          const r = await ctx.api.resolveRealm(region.value, name);
          if (request !== realmRequest) return;
          draft.realmSlug = r.slug;
        } catch (e) {
          if (request !== realmRequest) return;
          draft.realmSlug = "";
          ctx.toast(String(e), true);
        }
        paintResolved();
      }

      async function loadRealms() {
        const request = ++realmRequest;
        realm.replaceChildren(new Option("— pick a realm —", ""));
        try {
          const game = await ctx.api.detectGame(draft.wowRetailPath);
          if (request !== realmRequest) return;
          if (game.region === "eu" || game.region === "us") {
            draft.region = game.region;
            region.value = game.region;
          }
          realmNames = game.realmNames ?? [];
        } catch {
          if (request !== realmRequest) return;
          realmNames = [];
        }
        for (const name of realmNames) realm.add(new Option(name, name));
        if (realmNames.length > 0 && !draft.realmSlug) {
          realm.value = realmNames[0];
          await resolveSelected();
        } else {
          paintResolved();
        }
        if (realmNames.length === 0) {
          manual.hidden = false;
          manualToggle.hidden = true;
        }
      }

      region.addEventListener("change", () => {
        draft.region = region.value;
        resolveSelected();
      });
      realm.addEventListener("change", resolveSelected);
      manual.addEventListener("input", () => {
        realmRequest++;
        draft.realmSlug = manual.value.trim();
        paintResolved();
      });
      manualToggle.addEventListener("click", () => {
        manual.hidden = false;
        manualToggle.hidden = true;
        manual.focus();
      });

      card.append(head, regionLabel, region, realmLabel, realm, resolved, manualToggle, manual);
      bodyEl.append(card);
      loadRealms();
    }

    if (draft.foreverEnabled) {
      const card = document.createElement("div");
      card.className = "card stack";
      const head = document.createElement("div");
      head.className = "row";
      const title = document.createElement("span");
      title.className = "stage-title";
      title.textContent = "WoW: Forever";
      const headSpacer = document.createElement("span");
      headSpacer.className = "spacer";
      const ready = document.createElement("span");
      ready.className = "muted";
      ready.textContent = "ready";
      head.append(title, headSpacer, ready);
      const body = document.createElement("p");
      body.textContent =
        "Nothing to pick. Each scan says which server and faction it is from, and its prices go to that market.";
      card.append(head, body);
      bodyEl.append(card);
    }

    next.addEventListener("click", async () => {
      next.disabled = true;
      try {
        await ctx.api.saveConfig({ ...draft });
      } catch (e) {
        ctx.toast(String(e), true);
        paintNext();
        return;
      }
      go(2);
    });

    nav.replaceChildren(back, Object.assign(document.createElement("div"), { className: "spacer" }), next);
    paintNext();
  }

  // ---- step 2: connect ----------------------------------------------------

  function stepConnect() {
    setHead("Connect to goldcap.gg?", "So your sales show up as profit on the site.");
    bodyEl.replaceChildren();

    // Whichever way this step was reached, draft is already saved (Setup's
    // Next above saves before calling go(2)) — the sync itself waits for
    // finish() below, once the user actually leaves (Pair or Later).

    const back = document.createElement("button");
    back.className = "btn btn-ghost";
    back.type = "button";
    back.textContent = "← Back";
    back.addEventListener("click", () => go(1));

    const open = document.createElement("button");
    open.className = "btn";
    open.type = "button";
    open.textContent = "Open goldcap.gg/account ↗";
    open.addEventListener("click", () => ctx.api.openAccountPage().catch((e) => ctx.toast(String(e), true)));

    const label = document.createElement("label");
    label.className = "label";
    label.textContent = "Pairing code";
    label.htmlFor = "wizard-pair-code";

    const code = document.createElement("input");
    code.id = "wizard-pair-code";
    code.className = "field mono code-field";
    code.placeholder = "ABCD-1234";
    code.autocomplete = "off";
    code.spellcheck = false;
    code.addEventListener("input", () => {
      const raw = code.value;
      const caret = code.selectionStart ?? raw.length;
      // Reassigning `.value` below would otherwise fling the caret to the
      // end on every keystroke — fine for typing or pasting the code
      // straight through, but it makes correcting a mid-string typo
      // impossible. Re-derive the caret position from how many surviving
      // (alnum) characters sit before it, plus the hyphen formatPairCode
      // may insert at position 4.
      const kept = raw.slice(0, caret).replace(/[^A-Za-z0-9]/g, "").length;
      code.value = formatPairCode(raw);
      const pos = Math.min(kept > 4 ? kept + 1 : kept, code.value.length);
      code.setSelectionRange(pos, pos);
      pair.disabled = code.value.length < 9;
    });

    const hint = document.createElement("p");
    hint.className = "muted wizard-hint";
    hint.textContent = "The code is valid for 15 minutes.";

    const pair = document.createElement("button");
    pair.className = "btn btn-primary";
    pair.type = "button";
    pair.textContent = "Pair";
    pair.disabled = true;

    const later = document.createElement("button");
    later.className = "btn";
    later.type = "button";
    later.textContent = "Later";

    async function finish() {
      try {
        await ctx.api.syncNow();
      } catch {
        // The Status screen reports a failed sync properly; nothing useful to
        // add here, and it must not block leaving the wizard.
      }
      ctx.show("status");
    }

    pair.addEventListener("click", async () => {
      pair.disabled = true;
      try {
        await ctx.api.pairWithCode(code.value);
        ctx.toast("Paired with goldcap.gg");
        await finish();
      } catch (e) {
        ctx.toast(String(e), true);
        pair.disabled = false;
      }
    });
    later.addEventListener("click", finish);

    bodyEl.append(open, label, code, hint);
    nav.replaceChildren(back, later, Object.assign(document.createElement("div"), { className: "spacer" }), pair);
  }

  function render_step() {
    if (step === 0) stepGames();
    else if (step === 1) stepSetup();
    else stepConnect();
  }

  detect();
  return {};
}
