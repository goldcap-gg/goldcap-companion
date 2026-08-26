/**
 * The GoldCap mark — a coin inside a sniper crosshair — as inline SVG.
 *
 * The same geometry the site header, the favicon and the app icon draw
 * (apps/web/public/assets/favicon.svg is this drawing). The window used to
 * show an 18px CSS gradient circle instead: a gold blob that read as a
 * loading dot rather than as a logo, and at that size next to 15px text it
 * was barely there at all.
 */
export function brandMarkSvg(size = 26) {
  return `<svg class="brand-mark" width="${size}" height="${size}" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
  <circle cx="32" cy="32" r="14" fill="#9a751f"/>
  <circle cx="32" cy="32" r="11" fill="#f5c542"/>
  <path d="M25 27 A8 8 0 0 1 30 23" fill="none" stroke="#ffe08a" stroke-width="2.6" stroke-linecap="round"/>
  <circle cx="32" cy="32" r="22" fill="none" stroke="#f5c542" stroke-width="3.4"/>
  <g stroke="#f5c542" stroke-width="4" stroke-linecap="round">
    <line x1="32" y1="2" x2="32" y2="12"/><line x1="32" y1="52" x2="32" y2="62"/>
    <line x1="2" y1="32" x2="12" y2="32"/><line x1="52" y1="32" x2="62" y2="32"/>
  </g>
</svg>`;
}
