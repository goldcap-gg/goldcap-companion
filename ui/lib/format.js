// Pure formatting helpers. Everything the UI renders about time is computed
// here from a unix timestamp and a "now", so the screen can keep re-rendering
// between polls instead of showing a phrase the backend baked minutes ago.

export function relativeTime(at, now) {
  if (at === null || at === undefined) return "";
  const elapsed = Math.max(0, now - at);
  if (elapsed < 60) return "just now";
  if (elapsed < 3600) return `${Math.floor(elapsed / 60)} min ago`;
  if (elapsed < 86400) return `${Math.floor(elapsed / 3600)} h ago`;
  return `${Math.floor(elapsed / 86400)} d ago`;
}

export function countdown(until, now) {
  if (until === null || until === undefined) return "";
  const left = until - now;
  if (left <= 0) return "due";
  // Round up: a countdown that shows "0 min" for a whole minute reads as stuck.
  if (left < 5400) return `${Math.ceil(left / 60)} min`;
  return `${Math.round(left / 3600)} h`;
}

export function groupDigits(n) {
  // A thin space, not a comma: the mono digits are already wide enough.
  return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, "\u2009");
}

export function formatPairCode(raw) {
  const clean = raw.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 8);
  return clean.length > 4 ? `${clean.slice(0, 4)}-${clean.slice(4)}` : clean;
}

// One line per game folder the Companion found. Retail is only named: its own rows above say
// everything about it. WoW: Forever says what went up, what came back, and anything the player
// has to do; another game is named and left alone.
export function gameLine(g, now) {
  if (g.game === "retail") return `Retail · ${g.folder}`;
  if (g.game !== "forever") return `${g.folder} · not used`;
  const parts = [`WoW: Forever · ${g.folder}`];
  parts.push(g.lastSentAt ? `scan sent ${relativeTime(g.lastSentAt, now)}` : "no scan sent yet");
  if (g.crowdItems) parts.push(`${groupDigits(g.crowdItems)} prices from players, ${relativeTime(g.crowdTs, now)}`);
  if (g.note) parts.push(g.note);
  return parts.join(" · ");
}

// The Forever card's "scan uploaded" line: how long ago the last scan reached goldcap.gg and how
// many items it carried, or that nothing has gone up yet.
export function foreverScanLine(g, now) {
  if (!g.lastSentAt) return "No scan uploaded yet";
  const items = g.sentItems ? ` · ${groupDigits(g.sentItems)} items` : "";
  return `Scan uploaded ${relativeTime(g.lastSentAt, now)}${items}`;
}

// The Forever card's pairing line: everyone's prices come back without pairing (forever.rs asks
// goldcap.gg for the market of the install's newest scan), but the player's own scans go up only
// with a pairing token. Null once paired.
export function foreverPairLine(g, paired) {
  if (paired) return null;
  return "Everyone's prices come without pairing, after your first auction house scan. Pair this app with your GoldCap account to also share your own scans.";
}

// The Forever card's next step before anything has happened yet: the one thing to do in the game.
// Paired, that is until the first scan has gone up; unpaired, until there is a scan on disk to
// learn the market from. Null when the last scan was not sent (`g.skipped` says why and what to
// do instead).
export function foreverNextStep(g, paired) {
  if (g.skipped || (paired ? g.lastSentAt : g.lastScanAt)) return null;
  return "Next: open the auction house in WoW: Forever (GoldCap scans it), then type /reload.";
}

// The Status hero once Retail's own stages have had their say. A Forever scan the Companion did
// not send (`g.skipped`, the reason in the player's words, shown on the card) outranks "waiting"
// and "everything works": another scan alone will not fix it. Everyone's prices written to the
// addon count as working, paired or not.
export function foreverHeroPhrase(games, { retailEnabled }) {
  if (games.some((g) => g.unauthorized)) return "WoW: Forever needs attention";
  if (games.some((g) => g.skipped)) return "Your last WoW: Forever scan was not sent";
  if (retailEnabled || games.some((g) => g.lastSentAt || g.crowdWrittenAt)) return "Everything works";
  return "Waiting for your first scan";
}

// The Retail card's line when the game's own files point at another region than the saved one.
export function regionHintLine(hint, region) {
  return `Your game files say you play on ${hint.toUpperCase()}, but GoldCap is set to ${region.toUpperCase()}.`;
}

// How to get the code, the same words on the Status card and in Settings.
export const PAIR_HOW = "On goldcap.gg, sign in and open Account, press Show pairing code, and type the code in Settings here within 15 minutes.";

// The wizard's Connect step: what pairing adds. Prices come without it, in either game.
export function connectSubtitle(draft) {
  return draft.foreverEnabled
    ? "Pairing shares your WoW: Forever scans and shows your sales as profit on the site. Everyone's prices come without it."
    : "So your sales show up as profit on the site.";
}

// The Forever card's "crowd prices" line: when the companion last actually wrote everyone
// else's prices into this install's addon folder. Empty until that has happened once.
export function foreverCrowdLine(g, now) {
  return g.crowdWrittenAt ? `Crowd prices written to the addon ${relativeTime(g.crowdWrittenAt, now)}` : "";
}

// The Forever card's "market" subtitle: realm · faction, or nothing before the first accepted
// scan has named one.
export function foreverMarketLine(g) {
  return [g.realm, g.faction].filter(Boolean).join(" · ");
}

// The Forever card's "what your scan changed" block, from the site's counts for the last scan
// sent (`g.impact`, only ever set when goldcap.gg answered with numbers): `{ big, label, sub }`, or
// null when there are none — an old site, a quarantined scan — so the card shows nothing new.
// "updated" and "in the last 24 hours" are all the counts can prove: never "confirmed", never "live".
//   opened a market:   sub only  "You opened <market>: its first N prices are yours."
//   updated, some ours: big N, label "prices updated on <market>", sub "M of them nobody else had …"
//   updated, none ours: big N, label only
//   nothing updated:    sub only  "Your last scan updated no prices on <market>."
export function foreverImpactLines(g) {
  const i = g.impact;
  if (!i) return null;
  // A scan the game named no realm in, before goldcap.gg's prices for it named one.
  const market = i.realm ? foreverMarketLine(i) : "your realm";
  if (i.updated === 0) return { big: "", label: "", sub: `Your last scan updated no prices on ${market}.` };
  if (i.first) {
    return { big: "", label: "", sub: `You opened ${market}: its first ${groupDigits(i.updated)} prices are yours.` };
  }
  return {
    big: groupDigits(i.updated),
    label: `prices updated on ${market}`,
    sub: i.onlyYours > 0 ? `${groupDigits(i.onlyYours)} of them nobody else had in the last 24 hours.` : "",
  };
}

// True when version `a` ("1.2.3") is older than `b`, compared number by number.
function olderThan(a, b) {
  const pa = String(a).split(".").map(Number);
  const pb = String(b).split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) < (pb[i] || 0);
  }
  return false;
}

// The Status window's card for the GoldCap addon in one game install, from the Rust side's
// `AddonLine`: `{ title, detail, tone, error?, action? }`, or null when the Companion is not
// managing the addon there (the setting is off), so no card is drawn. `action` exists only for an
// addon another app keeps: the button that hands it over.
export function addonCard(line, now) {
  if (line.kind === "off") return null;
  const game = line.game === "forever" ? "WoW: Forever" : "Retail";
  const withVersion = (version) => `${game} · GoldCap addon${version ? ` ${version}` : ""}`;
  switch (line.kind) {
    case "managed":
      return {
        title: withVersion(line.installedVersion),
        detail: line.updatedAt
          ? `Kept up to date by the Companion. Updated ${relativeTime(line.updatedAt, now)}.`
          : "Installed by the Companion.",
        tone: "ok",
      };
    case "foreign":
      // No readable version means a linked or development copy: handing it over
      // could never do anything, so say why instead of offering the button.
      if (!line.installedVersion) {
        return {
          title: withVersion(""),
          detail: "GoldCap here is a linked or development copy, so the Companion leaves it alone.",
          tone: "idle",
        };
      }
      // A copy installed by hand is updated by nothing, so this never claims another app keeps it current.
      return {
        title: withVersion(line.installedVersion),
        detail:
          line.availableVersion && olderThan(line.installedVersion, line.availableVersion)
            ? `Another app or a manual install looks after GoldCap here. Addon ${line.availableVersion} is out.`
            : "Another app or a manual install looks after GoldCap here, so the Companion leaves it alone.",
        tone: "idle",
        action: { label: "Let the Companion update it", dir: line.dir },
      };
    case "takenOver":
      return {
        title: withVersion(line.installedVersion),
        detail: "The Companion will update it with the next release.",
        tone: "idle",
      };
    case "waiting":
      // An error here says why the Companion could not check the game, so the card does not blame the game.
      if (line.error) {
        return {
          title: withVersion(""),
          detail: `Addon ${line.availableVersion} is ready. The Companion cannot tell whether the game is open, so it waits.`,
          tone: "idle",
          error: line.error,
        };
      }
      return {
        title: withVersion(""),
        detail: `Addon ${line.availableVersion} is ready, it installs when you close the game.`,
        tone: "idle",
      };
    case "failed":
      // The Companion looks again in two minutes, then less often while it keeps failing.
      return {
        title: withVersion(""),
        detail: `The addon could not be ${line.installedVersion ? "updated" : "installed"} this time. The Companion will try again.`,
        tone: "broken",
        error: line.error ?? "",
      };
    default:
      return null;
  }
}

// True when one of an addon check's lines installed the addon at or after `since` (unix
// seconds, taken before the check). A line keeps its last change from earlier checks, so the
// wizard run a second time must not announce an install it did not make.
export function addonInstalledSince(lines, since) {
  return lines.some((l) => l.lastChange === "installed" && (l.updatedAt ?? 0) >= since);
}

// What ticking WoW: Forever in Settings saves when it has no folder yet: the root auto-detect
// finds (`detect`, what the wizard offers), else the folder the player picks (`pick`). Null when
// nothing was found and the picker was cancelled. A detection that fails falls back to the
// picker; a pick that fails throws, so its message reaches the player.
export async function foreverRootPatch(detect, pick) {
  let root = "";
  try {
    root = (await detect())?.foreverRootPath || "";
  } catch {
    /* the picker still works */
  }
  if (!root) root = (await pick()) || "";
  return root ? { foreverRootPath: root, foreverEnabled: true } : null;
}

// The Status screen's "next sync" line. An elapsed schedule is not "due": the loop is about to
// run (after a wake or a retry), so say that.
export function nextSyncLine(until, now) {
  const left = countdown(until, now);
  if (!left) return "";
  return left === "due" ? "Syncing soon" : `Next sync in ${left}`;
}
