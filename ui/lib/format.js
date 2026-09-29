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

export function truncateMiddle(text, max) {
  if (text.length <= max) return text;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = keep - head;
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
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
//   opened a market:   sub only  "You opened <market> — its first N prices are yours."
//   updated, some ours: big N, label "prices updated on <market>", sub "M of them nobody else had …"
//   updated, none ours: big N, label only
//   nothing updated:    sub only  "Your last scan updated no prices on <market>."
export function foreverImpactLines(g) {
  const i = g.impact;
  if (!i) return null;
  const market = foreverMarketLine(i);
  if (i.updated === 0) return { big: "", label: "", sub: `Your last scan updated no prices on ${market}.` };
  if (i.first) {
    return { big: "", label: "", sub: `You opened ${market} — its first ${groupDigits(i.updated)} prices are yours.` };
  }
  return {
    big: groupDigits(i.updated),
    label: `prices updated on ${market}`,
    sub: i.onlyYours > 0 ? `${groupDigits(i.onlyYours)} of them nobody else had in the last 24 hours.` : "",
  };
}
