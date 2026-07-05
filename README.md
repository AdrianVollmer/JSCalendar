# JSCalendar

A server-rendered JMAP calendar client, written in Rust.

It speaks [JMAP Core](https://www.rfc-editor.org/rfc/rfc8620) and
[JMAP for Calendars](https://www.rfc-editor.org/rfc/rfc9670) to talk to any
compliant server, and renders [JSCalendar](https://www.rfc-editor.org/rfc/rfc8984)
events (RFC 8984) into month/week/day/agenda views.

## Architecture

- **`crates/jmap-client`** — a standalone JMAP + JSCalendar library: session
  discovery, the Core request/response envelope, `Calendar`/`CalendarEvent`
  data types, recurrence-rule expansion, and IANA time zone conversion. No
  web framework dependency; usable on its own.
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

Then open `http://localhost:8787`, sign in with your JMAP server's URL (or
just its hostname — `/.well-known/jmap` is appended automatically),
username/password, or an API token.

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
- Light/dark theme (follows system preference, with a manual toggle
  persisted in `localStorage`).
- PWA: web app manifest, icons, and a service worker that caches the static
  app shell for instant loads and offline resilience (calendar data itself
  is always live — there's no offline data cache, since it belongs to your
  JMAP server).

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

## Development

```sh
cargo test --workspace   # jmap-client unit + integration tests
cargo clippy --workspace --all-targets
```

`crates/jmap-client/tests/integration.rs` exercises the client against a
mocked JMAP server (via `wiremock`) so it doesn't require a live account.
