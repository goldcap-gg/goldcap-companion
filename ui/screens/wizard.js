import { truncateMiddle, formatPairCode } from "../lib/format.js";

// The install path, region and realm all come out of the game's own files —
// see detect() below — so the stepped flow is only ever entered for
// whichever part detection could not answer. Steps 0 (Game) and 1 (Realm)
// are unchanged from the three-step wizard; step 2 (Connect) is both the
// pairing screen and, once detection lands, the very first thing the user
// sees. "Later" stays a full-weight sibling of "Pair" — leaving the ledger
// unconnected is a legitimate choice.
const STEPS = ["Game", "Realm", "Connect"];

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
    intervalMinutes: 30,
    launchAtStartup: false,
    companionToken: "",
  };
  let realmNames = [];
  let step = 2;

  // Whether the stepped flow (Game/Realm) has ever been entered this visit.
  // The progress bar has nothing to show a part-way-through position for
  // until it has — a pure-detection run never sees it at all.
  let enteredStepped = false;

  // Set by the "change" button right before it calls go(1) directly,
  // skipping Game — read and cleared by stepRealm() itself the instant it
  // starts running (see below), so it can never leak into a later, ordinary
  // Game → Realm visit. Only ever true for the one Realm instance it was
  // set for.
  let realmEnteredFromConnect = false;

  // The draft as of the last successful saveConfig — i.e. what Connect is
  // actually showing and what the sync loop is actually using. Only written
  // right after a save succeeds (detect()'s success path, and Realm's Next
  // handler below). Realm's Back button, when it returns straight to
  // Connect, restores this before showing it — a pick the user backed out
  // of without confirming via Next must not appear as if it had been.
  let savedDraft = null;

  const progress = document.createElement("div");
  progress.className = "progress";
  progress.hidden = true;
  for (const name of STEPS) {
    const seg = document.createElement("span");
    seg.className = "progress-seg";
    seg.title = name;
    progress.append(seg);
  }

  // The Connect screen's confirmation card. It is a persistent element (not
  // rebuilt per step, unlike bodyEl) because detect() paints it before any
  // step even exists — visible from the very first frame, in a quiet
  // "looking" state, and settles into the real verdict in place. That is
  // what keeps the detecting phase from flashing: there is only ever one
  // card, its content changes, the layout around it does not.
  const confirm = document.createElement("div");
  confirm.className = "card verdict connect-confirm";
  confirm.hidden = true;
  confirm.tabIndex = -1;
  const confirmHead = document.createElement("div");
  confirmHead.className = "row";
  const confirmDot = document.createElement("span");
  confirmDot.className = "dot dot-ok";
  const confirmLine = document.createElement("p");
  confirmHead.append(confirmDot, confirmLine);
  const confirmSub = document.createElement("p");
  confirmSub.className = "muted connect-confirm-sub";
  const change = document.createElement("button");
  change.className = "link connect-change";
  change.type = "button";
  change.textContent = "change";
  // "change" is only ever offered once the path resolved (paintConfirm
  // hides it otherwise), so a wrong realm is by far the common reason to
  // click it — walking through Game first would ask the user to confirm a
  // path that was never in question. Skip straight to Realm whenever the
  // path is already usable, using the exact condition Game's own Next uses
  // to enable itself; only fall back to Game when the path itself needs
  // fixing. stepRealm() reads and immediately clears
  // realmEnteredFromConnect (see below) so its own Back button knows which
  // screen it left.
  change.addEventListener("click", () => {
    const skipGame = draft.wowRetailPath.trim() !== "";
    realmEnteredFromConnect = skipGame;
    go(skipGame ? 1 : 0);
  });
  confirm.append(confirmHead, confirmSub, change);

  // Reflects `draft` onto the confirmation card. "Ready" is exactly the
  // condition that lets a config be saved (both path and slug present) —
  // before that it renders as the quiet placeholder line instead, with no
  // dot, no sub-line and no way to "change" a value that was never shown.
  function paintConfirm() {
    const ready = draft.realmSlug.trim() !== "" && draft.wowRetailPath.trim() !== "";
    confirm.classList.toggle("verdict-ok", ready);
    confirmDot.hidden = !ready;
    change.hidden = !ready;
    confirmLine.classList.toggle("mono", ready);
    confirmLine.classList.toggle("muted", !ready);
    confirmLine.textContent = ready
      ? `${draft.realmSlug} · ${draft.region.toUpperCase()}`
      : "Looking for World of Warcraft…";
    confirmSub.textContent = ready ? "Prices are already on their way to your addon." : "";
  }

  const head = document.createElement("div");
  head.className = "wizard-head";

  const bodyEl = document.createElement("div");
  bodyEl.className = "wizard-body stack";

  const nav = document.createElement("div");
  nav.className = "wizard-nav row";

  el.append(
    progress,
    confirm,
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
    progress.hidden = !enteredStepped;
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

  // Bumped on every navigation, including detect()'s own. detect() is a
  // single chain of sequential awaits with nothing else on screen to click
  // during it, so in practice nothing can outrace it today — but the same
  // discipline stepRealm's resolves already use is cheap to apply here too,
  // and it means a future escape hatch out of the detecting phase can't
  // reintroduce the class of bug this guards against.
  let requestGen = 0;

  function go(next) {
    requestGen++;
    if (next === 0 || next === 1) enteredStepped = true;
    step = next;
    confirm.hidden = next !== 2;
    bodyEl.classList.remove("step-in");
    render_step();
    paintProgress();
    // render_step() runs each stepX() function synchronously up to its
    // first await, and setHead() is always the first thing each one does —
    // so by this point the new heading already exists in the live DOM.
    headingEl?.focus();
    requestAnimationFrame(() => bodyEl.classList.add("step-in"));
  }

  // ---- detecting: runs on entry, before any step exists ------------------
  //
  // Everything the stepped flow would otherwise ask the user to confirm is
  // read straight out of the game's own files. Silent throughout — a failed
  // probe here is not an error the user needs to read, it just means the
  // corresponding step still exists to ask the question by hand.
  async function detect() {
    confirm.hidden = false;
    confirm.focus();
    paintConfirm();

    const gen = ++requestGen;

    let config = null;
    try {
      config = await ctx.api.getConfig();
    } catch {
      // Fall through to auto-detect below, same as an empty saved config.
    }
    if (gen !== requestGen) return;

    // Seed everything the wizard never asks about — interval, launch-at-
    // startup, and any pairing token already on disk — from what is
    // actually saved, before detection (or the stepped flow it falls back
    // to) touches anything. Only wowRetailPath, region and realmSlug are
    // ever overwritten below, and only once something has actually
    // determined them. Without this, a config from a previous run that this
    // pass reopens for — say, a realm that could not be detected — would
    // have every one of these silently reset the moment it saves: a paired
    // token wiped, launch-at-startup flipped on, back to a 30-minute
    // interval.
    if (config) {
      draft.intervalMinutes = config.intervalMinutes;
      draft.launchAtStartup = config.launchAtStartup;
      draft.companionToken = config.companionToken;
      draft.region = config.region;
      draft.realmSlug = config.realmSlug;
    }

    let path = (config?.wowRetailPath || "").trim();
    if (!path) {
      try {
        path = (await ctx.api.detectWowPath()) || "";
      } catch {
        path = "";
      }
    }
    if (gen !== requestGen) return;

    if (!path) {
      go(0);
      return;
    }
    draft.wowRetailPath = path;

    let game;
    try {
      game = await ctx.api.detectGame(path);
    } catch {
      game = { region: null, realmNames: [] };
    }
    if (gen !== requestGen) return;

    if (game.region === "eu" || game.region === "us") draft.region = game.region;
    realmNames = game.realmNames ?? [];
    // No realm names at all is a failure for this purpose even though
    // detectGame did not throw — the manual-slug fallback lives in Realm.
    if (realmNames.length === 0) {
      go(1);
      return;
    }

    try {
      const r = await ctx.api.resolveRealm(draft.region, realmNames[0]);
      if (gen !== requestGen) return;
      draft.realmSlug = r.slug;
    } catch {
      if (gen !== requestGen) return;
      go(1);
      return;
    }

    try {
      await ctx.api.saveConfig({ ...draft });
    } catch {
      if (gen !== requestGen) return;
      // Everything needed to save is already resolved and sitting in
      // draft; Realm's own Next button will retry the same save.
      go(1);
      return;
    }
    if (gen !== requestGen) return;

    savedDraft = { ...draft };
    go(2);
  }

  // ---- step 0: the game --------------------------------------------------

  async function stepGame() {
    setHead("Find World of Warcraft", "The companion writes prices into your addon folder.");
    bodyEl.replaceChildren();

    const verdict = document.createElement("div");
    verdict.className = "card verdict";
    const pathEl = document.createElement("p");
    pathEl.className = "mono verdict-path";
    verdict.append(pathEl);

    const choose = document.createElement("button");
    choose.className = "btn";
    choose.type = "button";
    choose.textContent = "Choose folder…";

    const next = document.createElement("button");
    next.className = "btn btn-primary";
    next.type = "button";
    next.textContent = "Next →";

    function paintPath() {
      const found = draft.wowRetailPath.trim() !== "";
      verdict.classList.toggle("verdict-ok", found);
      pathEl.textContent = found
        ? truncateMiddle(draft.wowRetailPath, 46)
        : "No install found — point at your World of Warcraft folder.";
      pathEl.title = draft.wowRetailPath;
      next.disabled = !found;
    }

    // Auto-detect below keeps running after the first paint. If the user
    // clicks "Choose folder…" before it resolves, their explicit pick must
    // win — a late auto-detect result must never clobber it.
    let userPicked = false;

    choose.addEventListener("click", async () => {
      try {
        const picked = await ctx.api.pickWowPath();
        if (picked) {
          userPicked = true;
          draft.wowRetailPath = picked;
          paintPath();
        }
      } catch (e) {
        ctx.toast(String(e), true);
      }
    });

    next.addEventListener("click", () => go(1));

    bodyEl.append(verdict, choose);
    nav.replaceChildren(Object.assign(document.createElement("div"), { className: "spacer" }), next);

    paintPath();
    if (!draft.wowRetailPath) {
      try {
        const detected = await ctx.api.detectWowPath();
        // Re-check both flags after the await: the user may have picked
        // their own folder (or this same call may have raced past a second
        // entry into the step) while detection was in flight.
        if (!userPicked && !draft.wowRetailPath) {
          draft.wowRetailPath = detected || "";
          paintPath();
        }
      } catch {
        // A failed auto-detect is not an error the user needs to read; the
        // "no install found" copy already says what to do.
      }
    }
  }

  // ---- step 1: the realm -------------------------------------------------

  async function stepRealm() {
    setHead("Pick your realm", "Read out of the game's own files.");
    bodyEl.replaceChildren();

    // Captured once, immediately, so this instance's own Back button knows
    // where it should go — and so the flag can never carry over into some
    // later, unrelated entry into this step (a normal Game → Next visit
    // never sets it, so it is already false by the time that happens; this
    // reset just makes it impossible to get that ordering wrong).
    const enteredFromConnect = realmEnteredFromConnect;
    realmEnteredFromConnect = false;

    const regionLabel = document.createElement("label");
    regionLabel.className = "label";
    regionLabel.textContent = "Region";
    regionLabel.htmlFor = "wizard-region";
    const region = document.createElement("select");
    region.className = "field";
    region.id = "wizard-region";
    region.add(new Option("EU", "eu"));
    region.add(new Option("US", "us"));
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

    const back = document.createElement("button");
    back.className = "btn btn-ghost";
    back.type = "button";
    back.textContent = "← Back";

    const next = document.createElement("button");
    next.className = "btn btn-primary";
    next.type = "button";
    next.textContent = "Next →";

    function paintResolved() {
      const ok = draft.realmSlug.trim() !== "";
      resolved.textContent = ok ? `→ ${draft.realmSlug}` : "";
      next.disabled = !ok;
    }

    // Bumped by anything that supersedes an in-flight lookup: picking a
    // different realm or region, switching to manual entry, or leaving the
    // step via Back/Next. A resolve (or the detectGame load below) that was
    // already in flight checks its own captured value against the current
    // one before writing anything, so a late response can never overwrite
    // what the user has done since.
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
      // The user has taken manual control — a resolve started before this
      // point must not land afterward and stomp what they typed.
      realmRequest++;
      draft.realmSlug = manual.value.trim();
      paintResolved();
    });
    manualToggle.addEventListener("click", () => {
      manual.hidden = false;
      manualToggle.hidden = true;
      manual.focus();
    });
    back.addEventListener("click", () => {
      realmRequest++;
      // A normal Game → Realm visit goes back to Game, same as always. But
      // when "change" skipped Game entirely because the path didn't need
      // fixing, Game was never part of this trip — sending Back there would
      // strand the user on a step with no Back button of its own (Game only
      // ever had a Next), one hop further from Connect than where they
      // started. Returning to Connect instead keeps Back a way out of
      // whatever screen the user is actually looking at, in every case.
      if (enteredFromConnect) {
        // Whatever was picked on this screen was never confirmed via Next —
        // Back cancels it. Restore the last actually-saved values first, so
        // Connect shows (and syncNow uses) what is really persisted rather
        // than a pick the user backed out of.
        if (savedDraft) Object.assign(draft, savedDraft);
        go(2);
      } else {
        go(0);
      }
    });
    next.addEventListener("click", async () => {
      try {
        await ctx.api.saveConfig({ ...draft });
      } catch (e) {
        ctx.toast(String(e), true);
        return;
      }
      savedDraft = { ...draft };
      realmRequest++;
      go(2);
    });

    bodyEl.append(regionLabel, region, realmLabel, realm, resolved, manualToggle, manual);
    nav.replaceChildren(back, Object.assign(document.createElement("div"), { className: "spacer" }), next);

    await loadRealms();
  }

  // ---- step 2: connect ----------------------------------------------------

  function stepConnect() {
    setHead("Connect to goldcap.gg?", "So your sales show up as profit on the site.");
    bodyEl.replaceChildren();
    paintConfirm();

    // Whichever way this step was reached, draft is already complete and
    // saved (detect() saves before calling go(2); Realm's Next above saves
    // before calling it too) — so a sync can start firing right away rather
    // than waiting for Pair/Later. Fire-and-forget: the Status screen is
    // where a failed sync gets reported, not here, and this must not block
    // the pairing UI from appearing.
    ctx.api.syncNow().catch(() => {});

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
    nav.replaceChildren(later, Object.assign(document.createElement("div"), { className: "spacer" }), pair);
  }

  function render_step() {
    if (step === 0) stepGame();
    else if (step === 1) stepRealm();
    else stepConnect();
  }

  detect();
  return {};
}
