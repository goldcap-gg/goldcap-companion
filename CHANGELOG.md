# GoldCap Companion

Release notes for the desktop companion. Written for the person running it,
not for the repository: what changed on screen, and what it means for your
data.

## 1.11.0 (unreleased)

- **Shopping lists now carry what it would cost to craft an item.** When
  goldcap.gg knows a recipe for something on your list, the Companion brings
  its reagents and their prices along too, so the BUY tab can say whether
  crafting it beats buying it — and turn the line into its reagents when it
  does. Needs addon 0.12.0.
- **Alerts and lists shared with you reach the game as well.** The items your
  price alerts are firing on right now, and a list someone shared with you,
  arrive as shopping runs of their own, each carrying the price ceiling it was
  set at and the realm it was seen on.

## 1.10.0 (unreleased)

- **Shopping lists arrive with prices.** A list from goldcap.gg now brings the
  site's price for each item along with it, so the addon's BUY tab knows what
  something usually costs — and what a vendor charges for it — even for items
  your price import has never carried. Needs addon 0.11.0.
- **What you buy from a list reaches your ledger on goldcap.gg.** Purchases made
  in the BUY tab now upload with the list they came from, so the site can total
  what a shopping run actually cost you.

## 1.9.0 (unreleased)

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
