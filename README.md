# JSCalendar

A server-rendered JMAP calendar client, written in Rust.

It speaks [JMAP Core](https://www.rfc-editor.org/rfc/rfc8620),
[JMAP for Calendars](https://www.rfc-editor.org/rfc/rfc9670), and JMAP
Contacts to talk to any compliant server, rendering
[JSCalendar](https://www.rfc-editor.org/rfc/rfc8984) events (RFC 8984) into
month/week/day/agenda views, plus [JSContact](https://www.rfc-editor.org/rfc/rfc9553)
(RFC 9553) contacts and their birthdays alongside them.

## Architecture

- **`crates/jmap-client`** — a standalone JMAP + JSCalendar/JSContact
  library: session discovery, the Core request/response envelope,
  `Calendar`/`CalendarEvent` and `AddressBook`/`Card` (contacts) data types,
  recurrence-rule expansion, birthday-anniversary expansion, and IANA time
  zone conversion. No web framework dependency; usable on its own.
- **`crates/server`** — an [axum](https://github.com/tokio-rs/axum) web app
  that renders HTML server-side with [Askama](https://github.com/askama-rs/askama)
  templates and layers [htmx](https://htmx.org) on top for snappy partial
  updates. There is no SPA framework and no build step for the frontend —
  every page and every htmx fragment is plain HTML, so the app is fully
  functional with JavaScript disabled (forms submit normally, links
  navigate normally); htmx just intercepts the same links/forms to swap
  content in place instead of doing a full page reload.

Credentials are never exposed to the browser: the server holds one JMAP
`Client` per logged-in session (in memory, keyed by an opaque session
cookie) and the browser only ever talks to the server.

## Running it

```sh
cargo run -p jscalendar-server
```

Environment variables:

| Variable            | Default                          | Meaning                                                             |
|---------------------|-----------------------------------|----------------------------------------------------------------------|
| `PORT`              | `8787`                            | HTTP port to listen on                                               |
| `JSCAL_TIMEZONE`    | `UTC`                             | IANA zone views are rendered in (e.g. `Europe/Berlin`)               |
| `JSCAL_STATIC_DIR`  | `crates/server/static`            | Where to serve `/static/*` and `/sw.js` from                         |
| `JSCAL_DEMO_SERVER_URL` | unset                         | If set, pre-fills the login form's server URL (see below)            |
| `JSCAL_DEMO_USERNAME`   | `demo`                        | Pre-filled username, only used when `JSCAL_DEMO_SERVER_URL` is set   |
| `JSCAL_DEMO_PASSWORD`   | `demo`                        | Pre-filled password, only used when `JSCAL_DEMO_SERVER_URL` is set   |
| `JSCAL_HOLIDAYS_REGION` | unset                         | German federal state to show a "Public Holidays" pseudo-calendar for (see below); unset means the feature doesn't appear at all |

Then open `http://localhost:8787`, sign in with your JMAP server's URL (or
just its hostname — `/.well-known/jmap` is appended automatically),
username/password, or an API token.

## Trying it without a JMAP account

`crates/mock-jmap-server` is a small in-memory JMAP server — calendars,
recurring events, and contacts with birthdays, seeded relative to today so
it always looks current — that accepts any credentials. It's a real
implementation of the wire protocol (session discovery, `Calendar`/
`CalendarEvent`/`AddressBook`/`ContactCard`, the `CalendarEvent/query`
result-reference chaining), not a UI fake, so it exercises the same
`jmap-client` code path a real server would.

```sh
make demo          # runs the app + mock server together, login pre-filled
```

or, fully containerized (no Rust toolchain needed):

```sh
make docker-demo-build
make docker-demo-run   # http://localhost:8787, login pre-filled
```

`make mock-server` runs just the mock server on its own (default
`:9090`) if you want to point a normal `cargo run -p jscalendar-server` at
it manually instead.

## What's implemented

- Session discovery, `Calendar/get`, `Calendar/set`, `CalendarEvent/get`,
  `CalendarEvent/query` + `CalendarEvent/get` chained via a JMAP result
  reference, `CalendarEvent/set`.
- JSCalendar `Event` properties: title, description, start/duration,
  time zone (including floating/no-zone events), all-day events, location,
  color, and recurrence rules.
- Recurrence expansion for `DAILY`/`WEEKLY`/`MONTHLY`/`YEARLY` with
  `interval`, `count`, `until`, `byDay` (including "2nd Tuesday"-style
  `nthOfPeriod`), and `byMonthDay`, plus `recurrenceOverrides` (exceptions
  and reschedules).
- Month, week, day, and agenda views, all addressable by URL
  (`/app?view=week&date=2026-07-06&cal=<visible-calendar-ids>`), with
  overlapping-event layout in the week/day grid.
- Create/edit/delete events through a dialog form, working both via htmx
  (no full page reload) and as a plain HTML form (no JavaScript at all).
- Create/edit/delete calendars (name, color, description) the same way, from
  a "+" next to the sidebar's "Calendars" heading and a gear icon on each
  calendar row; a newly created calendar becomes visible immediately.
- Light/dark theme (follows system preference, with a manual toggle
  persisted in `localStorage`).
- PWA: web app manifest, icons, and a service worker that caches the static
  app shell for instant loads and offline resilience (calendar data itself
  is always live — there's no offline data cache, since it belongs to your
  JMAP server).
- Contacts: a "Contacts" view (`AddressBook/get` + `ContactCard/get`) listing
  name/email, with a live-filter text field (matches name or email, updates
  as you type via htmx, degrades to submit-on-search without JavaScript) and
  create/edit/delete through the same dialog-form pattern as events
  (`ContactCard/set`). A **Birthdays** entry in the sidebar — toggle it like
  any other calendar — overlays each contact's `birth` anniversary (RFC 9553
  `anniversaries`) as a recurring, non-editable all-day item across
  month/week/day/agenda views, with the contact's age shown when their
  birth year is known. Both are hidden automatically if the JMAP server
  doesn't advertise Contacts support.
- Subscribed iCal-URL calendars: a "Subscriptions" section in the sidebar
  (separate from the real "Calendars" section) lets you add any public
  `.ics` URL as a read-only overlay calendar, with its own name/color and
  the same visibility toggle as everything else. These aren't JMAP
  calendars — the app has no database of its own, so subscriptions live
  only in memory for the running server process (lost on restart, same as
  login sessions) — each feed is fetched and parsed on demand and cached
  for `ICS_CACHE_TTL_SECS` (30 minutes) before being re-fetched, and a
  small ⚠ badge appears next to a subscription if its last fetch failed.
  The parser (`server/src/ics.rs`) covers `SUMMARY`/`DTSTART`/`DTEND`/
  `DURATION`, a common `RRULE` subset (`FREQ`/`INTERVAL`/`COUNT`/`UNTIL`/
  `BYDAY`/`BYMONTHDAY`/`BYMONTH`), and `EXDATE`; unrecognized recurrence
  shapes fall back to showing just the first occurrence rather than
  guessing.
- Public holidays: setting `JSCAL_HOLIDAYS_REGION` to a German federal state
  (e.g. `BadenWuerttemberg`) adds a read-only "Public Holidays"
  pseudo-calendar computed with the [`holiday_de`](https://crates.io/crates/holiday_de)
  crate — no network fetch, no stale data, correct for any year. It's
  opt-in per deployment (unset by default, so no region is silently assumed)
  and, unlike Birthdays, starts hidden even when configured — toggle it on
  from the sidebar the first time you want it.

## Known limitations

- The event editor only builds simple recurrence rules (a single frequency,
  interval, and end condition); it doesn't expose `byDay` weekday pickers or
  "nth weekday" UI, though events created elsewhere that use those are
  displayed correctly.
- Editing a single occurrence of a recurring event isn't exposed in the UI
  (edits apply to the whole series). `recurrenceOverrides` from other
  clients are still respected when rendering.
- The display time zone is a single server-wide setting (`JSCAL_TIMEZONE`),
  not a per-browser one, since there's no client-side JavaScript driving
  the server-rendered views.
- Participants/attendees, alerts, and sharing (`Calendar/set` `shareWith`)
  are modeled in `jmap-client` but not surfaced in the UI yet.
- ICS-subscription URLs aren't persisted to disk (see above), and the ICS
  parser ignores `VTIMEZONE` blocks — a `DTSTART` with a `TZID` parameter
  but no trailing `Z` is treated as floating rather than resolved against
  the named zone. `VALARM`, `ATTENDEE`, and per-instance `RECURRENCE-ID`
  overrides in a feed are ignored entirely.
- The built-in public-holidays pseudo-calendar only covers Germany (via
  `holiday_de`, one `GermanRegion` per deployment). There's no well-maintained
  Rust equivalent of Python's `holidays` package to draw on for other
  countries as of this writing — the closest match (`holidays` on crates.io)
  bakes in a static dataset that stops at 2023 and hasn't been updated since
  early 2023, so it can't produce correct dates for the current year, let
  alone future ones. A non-German deployment that wants this feature today
  has to fall back to the ICS-subscription mechanism above with a
  region-specific public holiday `.ics` feed instead.
- Contacts have no address-book management UI (creating/renaming address
  books), and editing a contact only exposes name, one email, and birthday —
  not the full JSContact object model. There's no JMAP filter for "has a
  birthday in this range" or "name/email contains", so `ContactCard/get`
  always fetches the whole address book (requesting only
  `uid`/`name`/`emails`/`anniversaries` via its `properties` argument, not
  full cards) and the text filter is applied server-side in memory rather
  than pushed down to the server; the fetched result is cached per account
  for `CONTACTS_CACHE_TTL_SECS` (15 minutes), invalidated immediately on any
  create/edit/delete, so calendar views don't re-fetch it on every render.
  Fine for a personal address book; a very large one would want incremental
  sync via `ContactCard/changes` instead of a blind TTL.

## Development

```sh
cargo test --workspace   # jmap-client unit + integration tests
cargo clippy --workspace --all-targets
```

`crates/jmap-client/tests/integration.rs` exercises the client against a
mocked JMAP server (via `wiremock`) so it doesn't require a live account.
