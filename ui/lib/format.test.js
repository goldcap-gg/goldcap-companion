import { test } from "node:test";
import assert from "node:assert/strict";
import {
  relativeTime,
  countdown,
  truncateMiddle,
  groupDigits,
  formatPairCode,
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
