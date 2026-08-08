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
  return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}

export function formatPairCode(raw) {
  const clean = raw.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 8);
  return clean.length > 4 ? `${clean.slice(0, 4)}-${clean.slice(4)}` : clean;
}
