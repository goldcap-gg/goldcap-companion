import { test } from "node:test";
import assert from "node:assert/strict";
import {
  relativeTime,
  countdown,
  groupDigits,
  formatPairCode,
  gameLine,
  foreverScanLine,
  foreverCrowdLine,
  foreverMarketLine,
  foreverImpactLines,
  addonCard,
  addonInstalledSince,
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
    sub: "You opened Classic Beta PvE 2 · Horde: its first 2\u2009210 prices are yours.",
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

const line = (over) => ({
  dir: "C:\\WoW\\_retail_\\Interface\\AddOns",
  folder: "_retail_",
  game: "retail",
  kind: "managed",
  installedVersion: "0.22.0",
  availableVersion: "0.22.0",
  updatedAt: null,
  lastChange: null,
  error: null,
  ...over,
});

test("addonCard: managed says when the Companion last updated it", () => {
  const c = addonCard(line({ updatedAt: NOW - 240, lastChange: "updated" }), NOW);
  assert.equal(c.title, "Retail · GoldCap addon 0.22.0");
  assert.equal(c.detail, "Kept up to date by the Companion. Updated 4 min ago.");
  assert.equal(c.tone, "ok");
  assert.equal(c.action, undefined);
});

test("addonCard: managed without a time says it was installed", () => {
  const c = addonCard(line(), NOW);
  assert.equal(c.detail, "Installed by the Companion.");
});

test("addonCard: foreign says another app or a manual install looks after it", () => {
  const c = addonCard(
    line({ kind: "foreign", game: "forever", folder: "_classic_beta_", installedVersion: "0.21.0", availableVersion: "0.21.0" }),
    NOW,
  );
  assert.equal(c.title, "WoW: Forever · GoldCap addon 0.21.0");
  assert.equal(c.detail, "Another app or a manual install looks after GoldCap here, so the Companion leaves it alone.");
});

test("addonCard: foreign behind the latest release says which one is out", () => {
  const c = addonCard(line({ kind: "foreign", installedVersion: "0.9.0", availableVersion: "0.23.0" }), NOW);
  assert.equal(c.detail, "Another app or a manual install looks after GoldCap here. Addon 0.23.0 is out.");
  const ahead = addonCard(line({ kind: "foreign", installedVersion: "0.24.0", availableVersion: "0.23.0" }), NOW);
  assert.doesNotMatch(ahead.detail, /is out/);
});

test("addonCard: foreign has the take-over action, naming the Companion as every other line does", () => {
  const c = addonCard(line({ kind: "foreign", installedVersion: "0.21.0" }), NOW);
  assert.deepEqual(c.action, { label: "Let the Companion update it", dir: line().dir });
});

test("addonCard: a foreign copy with no readable version has no button and says why", () => {
  const c = addonCard(line({ kind: "foreign", installedVersion: null }), NOW);
  assert.equal(c.action, undefined);
  assert.match(c.detail, /linked or development copy/);
});

test("addonCard: taken over names the installed version", () => {
  const c = addonCard(line({ kind: "takenOver", installedVersion: "0.21.0" }), NOW);
  assert.equal(c.title, "Retail · GoldCap addon 0.21.0");
  assert.equal(c.detail, "The Companion will update it with the next release.");
  assert.equal(c.action, undefined);
});

test("addonCard: waiting names the version that is ready", () => {
  const c = addonCard(line({ kind: "waiting", installedVersion: null, availableVersion: "0.23.0" }), NOW);
  assert.equal(c.title, "Retail · GoldCap addon");
  assert.equal(c.detail, "Addon 0.23.0 is ready, it installs when you close the game.");
});

test("addonCard: waiting because the game cannot be checked says so, not that the game is open", () => {
  const c = addonCard(
    line({ kind: "waiting", installedVersion: null, availableVersion: "0.23.0", error: "the list of running programs cannot be read: denied" }),
    NOW,
  );
  assert.equal(c.detail, "Addon 0.23.0 is ready. The Companion cannot tell whether the game is open, so it waits.");
  assert.equal(c.error, "the list of running programs cannot be read: denied");
  assert.equal(c.tone, "idle");
});

test("addonCard: failed says the Companion tries again and carries the error", () => {
  const c = addonCard(line({ kind: "failed", error: "could not move the new addon in: denied" }), NOW);
  assert.equal(c.detail, "The addon could not be updated this time. The Companion will try again.");
  assert.equal(c.error, "could not move the new addon in: denied");
  assert.equal(c.tone, "broken");
});

test("addonCard: a failed first install says installed, not updated", () => {
  const c = addonCard(line({ kind: "failed", installedVersion: null, error: "timed out" }), NOW);
  assert.equal(c.detail, "The addon could not be installed this time. The Companion will try again.");
});

test("addonInstalledSince: only an install from this check counts", () => {
  const since = NOW - 5;
  assert.equal(addonInstalledSince([line({ lastChange: "installed", updatedAt: NOW })], since), true);
  // Installed on an earlier pass: a wizard run again must not announce it again.
  assert.equal(addonInstalledSince([line({ lastChange: "installed", updatedAt: NOW - 3600 })], since), false);
  assert.equal(addonInstalledSince([line({ lastChange: "updated", updatedAt: NOW })], since), false);
  assert.equal(addonInstalledSince([], since), false);
});

test("addonCard: no card when the companion does not manage the addon", () => {
  assert.equal(addonCard(line({ kind: "off" }), NOW), null);
});
