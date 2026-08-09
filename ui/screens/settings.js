import { truncateMiddle, formatPairCode } from "../lib/format.js";

const PRESET_INTERVALS = [15, 30, 60];

// No Save button. Everything persists on change or blur — the old form saved
// pairing immediately but everything else only on Save, and nothing on
// screen explained the difference.
//
// Only one thing here is structural: the Account group, which swaps between
// two entirely different sets of controls depending on whether a token is
// present. Everything else (path, region, realm, interval, startup) only
// ever changes a value that is already on screen, so those groups are built
// once and updated in place — see the paintX() closures below. That is what
// keeps a keystroke in the custom-interval field, or a click on a segment,
// from throwing focus back to <body> the way a full body.replaceChildren()
// would.
// How long the "Unpair — click to confirm" armed state stays live before it
// reverts on its own.
const UNPAIR_CONFIRM_MS = 5000;

export function render(el, ctx) {
  el.classList.add("screen-settings", "screen-scroll");

  let config = null;
  // Set by dispose(). Every async handler below checks this right after its
  // own await(s) — the same rule status.js's refresh()/action click use —
  // so a save, detect, or pair that resolves after the user has already
  // navigated to another screen cannot toast, repaint, or persist over top
  // of it. A flat flag rather than wizard.js's requestGen counter: unlike
  // the wizard's single navigating sequence, these are independent
  // per-control operations with nothing to generation-number against each
  // other, exactly the shape status.js's own handlers are in.
  let disposed = false;
  // The one timer this screen owns: the Unpair confirm window. Shared across
  // paintAccount's closures so any rebuild of the Account group (or disposal
  // of the whole screen) can cancel a pending one — see buildAccount below.
  let unpairArmTimer = null;

  const top = document.createElement("div");
  top.className = "topbar glass topbar-sticky";
  const back = document.createElement("button");
  back.className = "icon-btn";
  back.type = "button";
  back.title = "Back";
  back.setAttribute("aria-label", "Back to status");
  back.textContent = "←";
  back.addEventListener("click", () => ctx.show("status"));
  const heading = document.createElement("h1");
  heading.className = "topbar-title";
  heading.textContent = "Settings";
  top.append(back, heading, document.createElement("span"));

  const body = document.createElement("div");
  body.className = "stack settings-body";

  el.append(top, body);

  // Applies `patch` on top of `config` immediately, synchronously, before
  // the save even goes out — not after it resolves. Two edits fired close
  // together (click "60 min", then toggle "Launch at startup" before the
  // first save returns) must both land: with the merge only happening after
  // await, the second call would build its patch from the pre-edit
  // snapshot and its save would overwrite the first edit right back out,
  // even though both saves reported success. Applying optimistically means
  // the second call's `next` already carries the first call's change.
  //
  // A rejected save unwinds only its own patch, and only if nothing newer
  // has been layered on top of it since (`config === next` — reference
  // equality, since `next` is a fresh object every call). A newer edit that
  // has already superseded this one must not be reverted by this one's
  // failure.
  async function persist(patch) {
    const previous = config;
    const next = { ...config, ...patch };
    config = next;
    try {
      await ctx.api.saveConfig(next);
      if (disposed) return true;
      ctx.toast("Saved");
      return true;
    } catch (e) {
      if (disposed) return false;
      if (config === next) config = previous;
      ctx.toast(String(e), true);
      return false;
    }
  }

  function group(title) {
    const section = document.createElement("section");
    section.className = "group";
    const h = document.createElement("h2");
    h.className = "group-title";
    h.textContent = title;
    section.append(h);
    body.append(section);
    return section;
  }

  // `input` is the one control the label actually names — for a row made of
  // several elements (the path row, the realm row) that is the field which
  // mirrors the persisted value, not the buttons beside it. `row`, when
  // given, is what actually gets appended; the label's `for` still points at
  // `input` by id, wherever it sits inside `row`.
  function fieldBlock(id, labelText, input, row = input) {
    const wrap = document.createElement("div");
    const label = document.createElement("label");
    label.className = "label";
    label.htmlFor = id;
    label.textContent = labelText;
    input.id = id;
    wrap.append(label, row);
    return wrap;
  }

  // ---- game ----------------------------------------------------------

  function buildGame() {
    const game = group("Game");

    const pathRow = document.createElement("div");
    pathRow.className = "row";

    // A real (readonly) input rather than a <span> — it is the one control
    // that mirrors config.wowRetailPath, exactly as the slug field mirrors
    // config.realmSlug below, so it is what "WoW retail folder" can
    // meaningfully label. Change/Detect are the row's other controls and
    // carry their own names.
    const pathInput = document.createElement("input");
    pathInput.type = "text";
    pathInput.className = "mono path-text";
    pathInput.readOnly = true;

    const rowSpacer = document.createElement("span");
    rowSpacer.className = "spacer";

    const change = document.createElement("button");
    change.className = "btn btn-sm";
    change.type = "button";
    change.textContent = "Change…";
    change.setAttribute("aria-label", "Change WoW retail folder");

    const detect = document.createElement("button");
    detect.className = "btn btn-sm";
    detect.type = "button";
    detect.textContent = "Detect";
    detect.setAttribute("aria-label", "Detect WoW retail folder automatically");

    pathRow.append(pathInput, rowSpacer, change, detect);
    game.append(fieldBlock("settings-wow-path", "WoW retail folder", pathInput, pathRow));

    function paintPath() {
      pathInput.value = truncateMiddle(config.wowRetailPath || "not set", 34);
      pathInput.title = config.wowRetailPath || "";
    }

    change.addEventListener("click", async () => {
      try {
        const picked = await ctx.api.pickWowPath();
        if (disposed || !picked) return;
        const ok = await persist({ wowRetailPath: picked });
        if (disposed) return;
        paintPath();
        // Only worth re-reading the game's realm list when the path that
        // was actually saved changed — a rejected save leaves config (and
        // so the path detectGame would read) exactly where it was.
        if (ok) loadRealmNames(config.wowRetailPath);
      } catch (e) {
        if (disposed) return;
        ctx.toast(String(e), true);
      }
    });

    detect.addEventListener("click", async () => {
      try {
        const found = await ctx.api.detectWowPath();
        if (disposed) return;
        if (!found) {
          ctx.toast("No install found", true);
          return;
        }
        const ok = await persist({ wowRetailPath: found });
        if (disposed) return;
        paintPath();
        if (ok) loadRealmNames(config.wowRetailPath);
      } catch (e) {
        if (disposed) return;
        ctx.toast(String(e), true);
      }
    });

    const region = document.createElement("select");
    region.className = "field";
    region.add(new Option("EU", "eu"));
    region.add(new Option("US", "us"));
    game.append(fieldBlock("settings-region", "Region", region));

    function paintRegion() {
      region.value = config.region;
    }

    region.addEventListener("change", async () => {
      await persist({ region: region.value });
      if (disposed) return;
      // Re-synced from config either way: on success this is a no-op (the
      // select already shows what was just picked), on rejection it snaps
      // the dropdown back rather than leaving it showing an unsaved region.
      paintRegion();
    });

    const realmRow = document.createElement("div");
    realmRow.className = "row";

    const realm = document.createElement("select");
    realm.className = "field";
    realm.id = "settings-realm-detected";
    // The slug field below is what the outer label names — this dropdown
    // needs its own accessible name, since a <select>'s name is never
    // derived from its options' text.
    realm.setAttribute("aria-label", "Realm detected from game files");
    realm.add(new Option("— from game files —", ""));

    const slug = document.createElement("input");
    slug.className = "field mono";
    slug.autocomplete = "off";
    slug.spellcheck = false;

    realmRow.append(realm, slug);
    game.append(fieldBlock("settings-realm-slug", "Realm", slug, realmRow));

    function paintRealm() {
      slug.value = config.realmSlug;
    }

    realm.addEventListener("change", async () => {
      if (!realm.value) return;
      try {
        const r = await ctx.api.resolveRealm(region.value, realm.value);
        if (disposed) return;
        await persist({ realmSlug: r.slug });
      } catch (e) {
        if (disposed) return;
        ctx.toast(String(e), true);
      } finally {
        if (!disposed) paintRealm();
      }
    });

    slug.addEventListener("blur", async () => {
      const value = slug.value.trim();
      if (!value || value === config.realmSlug) {
        paintRealm();
        return;
      }
      await persist({ realmSlug: value });
      if (disposed) return;
      paintRealm();
    });

    // Bumped by every call, including this one's own — the standard guard
    // (copied from the wizard's realmRequest) against a slow response
    // landing after a newer one (rapid Change→Change, or Change while an
    // earlier detectGame is still in flight) and re-populating a dropdown
    // that has since moved on to a different install.
    let realmRequest = 0;
    async function loadRealmNames(path) {
      const gen = ++realmRequest;
      realm.replaceChildren(new Option("— from game files —", ""));
      try {
        const g = await ctx.api.detectGame(path);
        if (disposed || gen !== realmRequest) return;
        for (const name of g.realmNames ?? []) realm.add(new Option(name, name));
      } catch {
        if (disposed || gen !== realmRequest) return;
        // An unreadable install just leaves the dropdown at its placeholder;
        // the slug field next to it is always usable.
      }
    }

    paintPath();
    paintRegion();
    paintRealm();
    // Populated once per screen entry, not per repaint — nothing below this
    // point rebuilds the Game group, so this is the only call site besides
    // the two above that persist a new path.
    loadRealmNames(config.wowRetailPath);
  }

  // ---- sync ------------------------------------------------------------

  function buildSync() {
    const sync = group("Sync");

    // A segmented control, not a single input — a <label for> can only ever
    // name one focusable target, so this group is named the way a radio
    // group is: a visible label plus role="group"/aria-labelledby, with the
    // custom field carrying its own aria-label for when it is the thing
    // actually focused.
    const intervalLabel = document.createElement("span");
    intervalLabel.className = "label";
    intervalLabel.id = "settings-interval-label";
    intervalLabel.textContent = "How often";

    const segs = document.createElement("div");
    segs.className = "segments";
    segs.setAttribute("role", "group");
    segs.setAttribute("aria-labelledby", "settings-interval-label");

    const custom = document.createElement("input");
    custom.className = "field mono custom-interval";
    custom.id = "settings-interval-custom";
    custom.type = "number";
    custom.min = "1";
    custom.step = "1";
    custom.setAttribute("aria-label", "Custom interval, in minutes");

    const presetSegs = [];
    for (const minutes of PRESET_INTERVALS) {
      const seg = document.createElement("button");
      seg.className = "segment";
      seg.type = "button";
      seg.textContent = `${minutes} min`;
      seg.dataset.minutes = String(minutes);
      seg.addEventListener("click", async () => {
        await persist({ intervalMinutes: minutes });
        if (disposed) return;
        paintInterval();
      });
      segs.append(seg);
      presetSegs.push(seg);
    }

    const customSeg = document.createElement("button");
    customSeg.className = "segment";
    customSeg.type = "button";
    customSeg.textContent = "Custom";
    customSeg.addEventListener("click", () => {
      // Reveal-only: nothing is persisted until the field is actually
      // edited and blurred, below.
      custom.hidden = false;
      custom.focus();
    });
    segs.append(customSeg);

    custom.addEventListener("blur", async () => {
      const minutes = Number.parseInt(custom.value, 10);
      if (!Number.isFinite(minutes) || minutes < 1) {
        ctx.toast("Interval must be at least 1 minute", true);
        paintInterval();
        return;
      }
      if (minutes === config.intervalMinutes) {
        paintInterval();
        return;
      }
      await persist({ intervalMinutes: minutes });
      if (disposed) return;
      paintInterval();
    });

    // The single source of truth for which segment is lit, whether the
    // custom field is open, and what it shows — called after every attempt
    // (valid save, invalid revert, or rejected save) so the lit segment
    // always matches config.intervalMinutes, never an in-flight guess.
    function paintInterval() {
      const minutes = config.intervalMinutes;
      const isPreset = PRESET_INTERVALS.includes(minutes);
      for (const seg of presetSegs) {
        const on = Number(seg.dataset.minutes) === minutes;
        seg.classList.toggle("on", on);
        seg.setAttribute("aria-pressed", String(on));
      }
      customSeg.classList.toggle("on", !isPreset);
      customSeg.setAttribute("aria-pressed", String(!isPreset));
      custom.hidden = isPreset;
      custom.value = String(minutes);
    }

    const intervalWrap = document.createElement("div");
    intervalWrap.className = "stack";
    intervalWrap.append(intervalLabel, segs, custom);
    sync.append(intervalWrap);

    const startup = document.createElement("label");
    startup.className = "checkbox-row";
    const startupBox = document.createElement("input");
    startupBox.type = "checkbox";
    startupBox.id = "settings-launch-startup";
    startupBox.addEventListener("change", async () => {
      await persist({ launchAtStartup: startupBox.checked });
      if (disposed) return;
      paintStartup();
    });
    startup.append(startupBox, document.createTextNode("Launch at startup"));
    sync.append(startup);

    function paintStartup() {
      startupBox.checked = Boolean(config.launchAtStartup);
    }

    paintInterval();
    paintStartup();
  }

  // ---- account -----------------------------------------------------------
  //
  // The one group that is genuinely structural: pairing and unpairing swap
  // in a different set of controls, so this is the one place a rebuild is
  // correct rather than a bug. `paintAccount` replaces only accountBody
  // (the "Account" heading itself is never torn down) and, when told to,
  // moves focus to the state line — the same "focus the thing that just
  // changed" idiom the wizard uses on its step heading — so a screen reader
  // announces the new state instead of losing focus to <body>.

  let paintAccount = () => {};

  function buildAccount() {
    const account = group("Account");
    const accountBody = document.createElement("div");
    accountBody.className = "stack";
    account.append(accountBody);

    paintAccount = (focus = false) => {
      // Any rebuild — pair, unpair, or the initial paint — cancels a pending
      // Unpair confirm window. The button it belonged to is about to be torn
      // down either way; a control that stays armed after the user has
      // navigated elsewhere (or after the very unpair it was arming for has
      // already happened) is a trap.
      clearTimeout(unpairArmTimer);
      unpairArmTimer = null;

      accountBody.replaceChildren();
      const paired = config.companionToken.trim() !== "";

      const state = document.createElement("p");
      state.className = "muted account-state";
      state.tabIndex = -1;
      state.textContent = paired
        ? "Paired — your ledger uploads on every sync."
        : "Not paired — nothing is uploaded.";
      accountBody.append(state);

      if (paired) {
        // Unpairing is silent and not fully recoverable — the ledger the
        // addon is holding is capped and WoW rewrites SavedVariables at
        // logout, so an accidental unpair left unnoticed for long enough
        // loses real data, not just a setting. No modal exists anywhere in
        // this design, so the confirm is inline: the button becomes its own
        // confirmation. The button's visible text IS its accessible name
        // (no separate aria-label) specifically so the armed state is
        // announced the same way it's shown — nothing to keep in sync by
        // hand.
        const drop = document.createElement("button");
        drop.className = "btn";
        drop.type = "button";
        drop.textContent = "Unpair";

        function disarm() {
          // Disabling a focused button fires `blur` in Chromium and
          // Firefox — without this guard, the confirming click above
          // (which disables `drop` for the duration of the request) would
          // revert its own label via this same handler, for the in-flight
          // moment before the request resolves. The confirm was accepted,
          // not cancelled; `drop.disabled = false` in the catch below
          // clears this guard before calling disarm() for real.
          if (drop.disabled) return;
          clearTimeout(unpairArmTimer);
          unpairArmTimer = null;
          drop.classList.remove("btn-danger");
          drop.textContent = "Unpair";
        }

        drop.addEventListener("blur", disarm);

        drop.addEventListener("click", async () => {
          if (unpairArmTimer === null) {
            drop.classList.add("btn-danger");
            drop.textContent = "Unpair — click to confirm";
            unpairArmTimer = setTimeout(disarm, UNPAIR_CONFIRM_MS);
            return;
          }

          clearTimeout(unpairArmTimer);
          unpairArmTimer = null;
          drop.disabled = true;
          try {
            await ctx.api.unpair();
            if (disposed) return;
            config = { ...config, companionToken: "" };
            ctx.toast("Unpaired");
            paintAccount(true);
          } catch (e) {
            if (disposed) return;
            ctx.toast(String(e), true);
            drop.disabled = false;
            disarm();
          }
        });
        accountBody.append(drop);
      } else {
        const open = document.createElement("button");
        open.className = "btn";
        open.type = "button";
        open.textContent = "Open goldcap.gg/account ↗";
        open.addEventListener("click", () =>
          ctx.api.openAccountPage().catch((e) => ctx.toast(String(e), true)),
        );

        const code = document.createElement("input");
        code.className = "field mono code-field";
        code.placeholder = "ABCD-1234";
        code.autocomplete = "off";
        code.spellcheck = false;

        const pair = document.createElement("button");
        pair.className = "btn btn-primary";
        pair.type = "button";
        pair.textContent = "Pair";
        pair.disabled = true;

        code.addEventListener("input", () => {
          code.value = formatPairCode(code.value);
          pair.disabled = code.value.length < 9;
        });
        pair.addEventListener("click", async () => {
          pair.disabled = true;
          try {
            await ctx.api.pairWithCode(code.value);
            if (disposed) return;
            // The real token never round-trips back into the UI's own
            // patches, so a full getConfig() is the only way to see it.
            config = await ctx.api.getConfig();
            if (disposed) return;
            ctx.toast("Paired with goldcap.gg");
            paintAccount(true);
          } catch (e) {
            if (disposed) return;
            ctx.toast(String(e), true);
            pair.disabled = false;
          }
        });

        accountBody.append(open, fieldBlock("settings-pair-code", "Pairing code", code), pair);
      }

      if (focus) state.focus();
    };

    paintAccount();
  }

  ctx.api
    .getConfig()
    .then((c) => {
      if (disposed) return;
      config = c;
      buildGame();
      buildSync();
      buildAccount();
    })
    .catch((e) => {
      if (disposed) return;
      ctx.toast(String(e), true);
    });

  return {
    // The Unpair confirm window is the only timer this screen ever owns.
    // Without this, navigating away (back arrow → Status) while armed would
    // leave a stray setTimeout pointed at a detached button — harmless in
    // practice, but not something a screen should leave running. `disposed`
    // is what stops every other pending async handler (persist, detect,
    // pair, unpair, the initial load) from acting on this screen once it's
    // gone — see the declaration above.
    dispose() {
      disposed = true;
      clearTimeout(unpairArmTimer);
    },
  };
}
