# GoldCap Companion

Release notes for the desktop companion. Written for the person running it,
not for the repository: what changed on screen, and what it means for your
data.

## 1.13.0 (unreleased)

- The Companion now hands the addon the prices of your alert groups — every
  item with a price you set, or a target the site computed — so the in-game
  sniper can watch them live at the auction house. When an alert fires on
  gear you gave a minimum item level, its line in the BUY tab shows that item
  level too. Needs the matching addon release; older addons ignore the extra
  data.
- The Companion now also brings the prices of every commodity in your region,
  and how well each one sells, not just the few hundred busiest, so the addon's
  tooltips and sniper cover the whole market. The Status screen says how many
  items that was and how fresh they are. Needs the matching addon release;
  older addons ignore it.

## 1.12.0 (2026-09-20)

- **You decide when the Companion looks for updates.** Settings has a new
  "Check for updates automatically" box. Untick it and the Companion never
  checks for or downloads a new version on its own; the "Check for updates"
  button under it does that when you ask. It stays on unless you change it.
  On Windows an update has always waited for your click before installing —
  that part is unchanged.

## 1.11.0 (2026-09-16)

- **Shopping lists now carry what it would cost to craft an item.** When
  goldcap.gg knows a recipe for something on your list, the Companion brings
  its reagents and their prices along too, so the BUY tab can say whether
  crafting it beats buying it — and turn the line into its reagents when it
  does. Needs addon 0.12.0.
- **Alerts and lists shared with you reach the game as well.** The items your
  price alerts are firing on right now arrive as a shopping run of their own,
  each carrying the price ceiling it was set at and the realm it was seen on.
  A list someone shared with you arrives as a shopping run too.

- **Shopping lists arrive with prices.** A list from goldcap.gg now brings the
  site's price for each item along with it, so the addon's BUY tab knows what
  something usually costs — and what a vendor charges for it — even for items
  your price import has never carried. Needs addon 0.11.0.
- **What you buy from a list reaches your ledger on goldcap.gg.** Purchases made
  in the BUY tab now upload with the list they came from, so the site can total
  what a shopping run actually cost you.

- **Your saved lists reach the addon.** Lists with quantities saved on goldcap.gg
  (a profession's shopping list, for one) show up in the addon's BUY tab next
  time you log in or `/reload`. Needs addon 0.10.0.

## 1.8.0 (2026-09-08)

- **Sends goldcap.gg your posted and cancelled auctions.** With addon 0.8.0, the game
  remembers every lot your Auction House tab has seen, and the Companion uploads it on the
  regular sync tick — the same way ledger sales already sync. Powers the new My auctions page
  on the site. Nothing new to set up.

## 1.7.2 (2026-08-29)

- **The update banner no longer pushes the Sync now button off the bottom of
  the window.** When an update was ready, the "Sync now" button could end up
  half cut off below the edge of the window.
- **Sends the site the names of items it could not name.** With addon 0.6.2,
  the game resolves a short list of hotfix-only items (Tuskarr Jerky and about
  a hundred more) and the Companion passes those names to goldcap.gg on its
  next sync, so they show up in search there. Nothing new to set up.

## 1.7.1 (2026-08-28)

- **Written release notes start here.** Every version up to this one shipped
  without them. The companion kept working the way it always has — it watches
  your WoW folder, uploads what the addon recorded, and writes the market
  snapshot back so the addon has fresh prices — and from here on, each release
  says what changed in it.
