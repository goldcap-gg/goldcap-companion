import { test } from "node:test";
import assert from "node:assert/strict";
import {
  relativeTime,
  countdown,
  truncateMiddle,
  groupDigits,
  formatPairCode,
  gameLine,
  foreverScanLine,
  foreverCrowdLine,
  foreverMarketLine,
  foreverImpactLines,
} from "./format.js";

const NOW = 1_785_600_000;

test("relativeTime says 'just now' inside a minute", () => {
  assert.equal(relativeTime(NOW - 5, NOW), "just now");
  assert.equal(relativeTime(NOW - 59, NOW), "just now");
});

test("relativeTime counts minutes, hours and days", () => {
  assert.equal(relativeTime(NOW - 60, NOW), "1 min ago");
  assert.equal(relativeTime(NOW - 240, NOW), "4 min ago");
  assert.equal(relativeTime(NOW - 3 * 3600, NOW), "3 h ago");
  assert.equal(relativeTime(NOW - 2 * 86400, NOW), "2 d ago");
});

test("relativeTime treats a clock skewed into the future as now", () => {
  assert.equal(relativeTime(NOW + 120, NOW), "just now");
});

test("relativeTime returns an empty string for a missing timestamp", () => {
  assert.equal(relativeTime(null, NOW), "");
  assert.equal(relativeTime(undefined, NOW), "");
});

test("countdown rounds up so it never shows a stale zero", () => {
  assert.equal(countdown(NOW + 1560, NOW), "26 min");
  assert.equal(countdown(NOW + 61, NOW), "2 min");
  assert.equal(countdown(NOW + 30, NOW), "1 min");
});

test("countdown reports an elapsed schedule as due", () => {
  assert.equal(countdown(NOW - 10, NOW), "due");
  assert.equal(countdown(null, NOW), "");
});

test("countdown switches to hours past 90 minutes", () => {
  assert.equal(countdown(NOW + 2 * 3600, NOW), "2 h");
});

test("truncateMiddle keeps both ends of a long path", () => {
  const path = "C:\\Program Files (x86)\\World of Warcraft\\_retail_";
  const out = truncateMiddle(path, 30);
  assert.equal(out.length, 30);
  assert.ok(out.startsWith("C:\\Prog"));
  assert.ok(out.endsWith("_retail_"));
  assert.ok(out.includes("…"));
});

test("truncateMiddle leaves short text alone", () => {
  assert.equal(truncateMiddle("/tmp/wow", 30), "/tmp/wow");
});

test("groupDigits uses thin separators", () => {
  // U+2009 THIN SPACE, spelled out — a literal space here would pass in the
  // editor and fail in the runner.
  assert.equal(groupDigits(142), "142");
  assert.equal(groupDigits(1245300), "1\u2009245\u2009300");
  assert.equal(groupDigits(0), "0");
});

test("formatPairCode upper-cases and hyphenates as you type", () => {
  assert.equal(formatPairCode("abcd1234"), "ABCD-1234");
  assert.equal(formatPairCode("abcd-1234"), "ABCD-1234");
  assert.equal(formatPairCode("ab"), "AB");
  assert.equal(formatPairCode("abcde"), "ABCD-E");
});

test("formatPairCode drops junk and caps the length", () => {
  assert.equal(formatPairCode("ab cd!12 34xyz"), "ABCD-1234");
});

test("gameLine: retail says only where it is", () => {
  assert.equal(gameLine({ folder: "_retail_", game: "retail" }, 1000), "Retail · _retail_");
});

test("gameLine: Forever says what was sent, what came back and anything the player must do", () => {
  const now = 1_790_000_000;
  assert.equal(
    gameLine({ folder: "_classic_beta_", game: "forever", lastSentAt: now - 300, crowdItems: 1974, crowdTs: now - 600,
      note: "Link Battle.net on goldcap.gg for your scans to count in public prices" }, now),
    "WoW: Forever · _classic_beta_ · scan sent 5 min ago · 1 974 prices from players, 10 min ago · Link Battle.net on goldcap.gg for your scans to count in public prices",
  );
  assert.equal(gameLine({ folder: "_classic_beta_", game: "forever" }, now), "WoW: Forever · _classic_beta_ · no scan sent yet");
});

test("gameLine: another game is named and left alone", () => {
  assert.equal(gameLine({ folder: "_ptr_", game: "other" }, 1), "_ptr_ · not used");
});

test("foreverScanLine reports the last scan's age and item count", () => {
  const now = 1_790_000_000;
  assert.equal(
    foreverScanLine({ lastSentAt: now - 300, sentItems: 1974 }, now),
    "Scan uploaded 5 min ago · 1 974 items",
  );
  assert.equal(foreverScanLine({ lastSentAt: now - 300 }, now), "Scan uploaded 5 min ago");
  assert.equal(foreverScanLine({}, now), "No scan uploaded yet");
});

test("foreverCrowdLine is empty until something has actually been written", () => {
  const now = 1_790_000_000;
  assert.equal(foreverCrowdLine({}, now), "");
  assert.equal(
    foreverCrowdLine({ crowdWrittenAt: now - 60 }, now),
    "Crowd prices written to the addon 1 min ago",
  );
});

test("foreverMarketLine joins realm and faction, or says nothing yet", () => {
  assert.equal(foreverMarketLine({ realm: "Classic Beta PvE 2", faction: "Horde" }), "Classic Beta PvE 2 · Horde");
  assert.equal(foreverMarketLine({ realm: "Classic Beta PvE 2" }), "Classic Beta PvE 2");
  assert.equal(foreverMarketLine({}), "");
});

const impact = (over) => ({
  at: 1790464249, sentAt: 1790464300, updated: 412, onlyYours: 38, first: false,
  realm: "Classic Beta PvE 2", faction: "Alliance", ...over,
});

test("foreverImpactLines is null without an impact, so the card shows nothing new", () => {
  assert.equal(foreverImpactLines({}), null);
  assert.equal(foreverImpactLines({ impact: null }), null);
});

test("foreverImpactLines: prices updated, with how many nobody else had", () => {
  assert.deepEqual(foreverImpactLines({ impact: impact() }), {
    big: "412",
    label: "prices updated on Classic Beta PvE 2 · Alliance",
    sub: "38 of them nobody else had in the last 24 hours.",
  });
});

test("foreverImpactLines: nobody-else count is left out when it is zero", () => {
  assert.deepEqual(foreverImpactLines({ impact: impact({ onlyYours: 0 }) }), {
    big: "412",
    label: "prices updated on Classic Beta PvE 2 · Alliance",
    sub: "",
  });
});

test("foreverImpactLines: the first scan on a market says the player opened it", () => {
  assert.deepEqual(foreverImpactLines({ impact: impact({ first: true, updated: 2210, onlyYours: 2210, faction: "Horde" }) }), {
    big: "",
    label: "",
    sub: "You opened Classic Beta PvE 2 · Horde — its first 2\u2009210 prices are yours.",
  });
});

test("foreverImpactLines: a scan that updated nothing says so, even on an opened market", () => {
  const sub = "Your last scan updated no prices on Classic Beta PvE 2 · Alliance.";
  assert.deepEqual(foreverImpactLines({ impact: impact({ updated: 0, onlyYours: 0 }) }), { big: "", label: "", sub });
  assert.deepEqual(foreverImpactLines({ impact: impact({ updated: 0, onlyYours: 0, first: true }) }), { big: "", label: "", sub });
});

test("foreverImpactLines groups digits and names a market without a faction by its realm alone", () => {
  const lines = foreverImpactLines({ impact: impact({ updated: 12345, onlyYours: 1200, faction: null }) });
  assert.equal(lines.big, "12\u2009345");
  assert.equal(lines.label, "prices updated on Classic Beta PvE 2");
  assert.equal(lines.sub, "1\u2009200 of them nobody else had in the last 24 hours.");
  assert.equal(foreverImpactLines({ impact: impact({ updated: 1, onlyYours: 1, faction: undefined }) }).big, "1");
});
