import { truncateMiddle, formatPairCode } from "../lib/format.js";

// Three steps. Pairing is the third and is genuinely optional — "Later" is a
// full-weight sibling of "Pair", not a small link, because leaving the ledger
// unconnected is a legitimate choice. The Status screen keeps the invitation.
const STEPS = ["Game", "Realm", "Pairing"];

export function render(el, ctx) {
  el.classList.add("screen-wizard");

  const draft = {
    region: "eu",
    realmSlug: "",
    wowRetailPath: "",
    intervalMinutes: 30,
    launchAtStartup: true,
    companionToken: "",
  };
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

  el.append(progress, head, bodyEl, Object.assign(document.createElement("div"), { className: "spacer" }), nav);

  function paintProgress() {
    [...progress.children].forEach((seg, i) => {
      seg.classList.toggle("done", i < step);
      seg.classList.toggle("current", i === step);
    });
  }

  function setHead(title, sub) {
    head.replaceChildren();
    const h = document.createElement("h1");
    h.textContent = title;
    const p = document.createElement("p");
    p.className = "muted wizard-sub";
    p.textContent = sub;
    head.append(h, p);
  }

  function go(next) {
    step = next;
    bodyEl.classList.remove("step-in");
    render_step();
    paintProgress();
    requestAnimationFrame(() => bodyEl.classList.add("step-in"));
  }

  // ---- step 1: the game --------------------------------------------------

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

  // ---- step 2: the realm -------------------------------------------------

  async function stepRealm() {
    setHead("Pick your realm", "Read out of the game's own files.");
    bodyEl.replaceChildren();

    const regionLabel = document.createElement("label");
    regionLabel.className = "label";
    regionLabel.textContent = "Region";
    const region = document.createElement("select");
    region.className = "field";
    region.add(new Option("EU", "eu"));
    region.add(new Option("US", "us"));
    region.value = draft.region;

    const realmLabel = document.createElement("label");
    realmLabel.className = "label";
    realmLabel.textContent = "Realm";
    const realm = document.createElement("select");
    realm.className = "field";

    const resolved = document.createElement("p");
    resolved.className = "mono resolved";

    const manual = document.createElement("input");
    manual.className = "field mono";
    manual.placeholder = "realm slug, e.g. dentarg";
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

    async function resolveSelected() {
      const name = realm.value;
      if (!name) return;
      try {
        const r = await ctx.api.resolveRealm(region.value, name);
        draft.realmSlug = r.slug;
      } catch (e) {
        draft.realmSlug = "";
        ctx.toast(String(e), true);
      }
      paintResolved();
    }

    async function loadRealms() {
      realm.replaceChildren(new Option("— pick a realm —", ""));
      try {
        const game = await ctx.api.detectGame(draft.wowRetailPath);
        if (game.region === "eu" || game.region === "us") {
          draft.region = game.region;
          region.value = game.region;
        }
        realmNames = game.realmNames ?? [];
      } catch {
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
      draft.realmSlug = manual.value.trim();
      paintResolved();
    });
    manualToggle.addEventListener("click", () => {
      manual.hidden = false;
      manualToggle.hidden = true;
      manual.focus();
    });
    back.addEventListener("click", () => go(0));
    next.addEventListener("click", async () => {
      try {
        await ctx.api.saveConfig({ ...draft });
      } catch (e) {
        ctx.toast(String(e), true);
        return;
      }
      go(2);
    });

    bodyEl.append(regionLabel, region, realmLabel, realm, resolved, manualToggle, manual);
    nav.replaceChildren(back, Object.assign(document.createElement("div"), { className: "spacer" }), next);

    await loadRealms();
  }

  // ---- step 3: pairing ---------------------------------------------------

  function stepPairing() {
    setHead("Connect your account", "So your sales show up as profit on goldcap.gg.");
    bodyEl.replaceChildren();

    const open = document.createElement("button");
    open.className = "btn";
    open.type = "button";
    open.textContent = "Open goldcap.gg/account ↗";
    open.addEventListener("click", () => ctx.api.openAccountPage().catch((e) => ctx.toast(String(e), true)));

    const label = document.createElement("label");
    label.className = "label";
    label.textContent = "Pairing code";

    const code = document.createElement("input");
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
    else stepPairing();
  }

  go(0);
  return {};
}
