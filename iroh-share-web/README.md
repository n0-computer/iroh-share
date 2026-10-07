# iroh-share-web

An experimental browser version of the iroh-share desktop GUI: an iroh endpoint
compiled to WebAssembly that pairs with a daemon and drives it over the control
protocol. It has the same layout and wording as the desktop GUI.

Browsers cannot open UDP sockets, so the endpoint is **relay-only** and reaches
the daemon through an n0 relay.

## Build and run

```bash
npm run build   # cargo (release, wasm32) → wasm-bindgen → self-contained dist/iroh-share/
npm run serve   # http://localhost:8080/iroh-share/
```

`dist/iroh-share/` is a static directory you can serve from anywhere, or
publish with `sendme send dist/iroh-share` and import it into a daemon. Paste a pairing ticket
from the daemon (`iroh-share control pair`), or open the page with the ticket in
the fragment: `http://localhost:8080/iroh-share/#ticket=irohshare…`. The fragment is never
sent to the server, and the page removes it from the address bar right away.

Requires the `wasm32-unknown-unknown` target and a `wasm-bindgen` CLI that
matches the pinned `wasm-bindgen` crate version in [Cargo.toml](Cargo.toml).
`npm run build:debug` builds a debug module instead.

## How it is split

All interface logic is Rust:

- `src/app.rs`: the state machine, ported from the desktop GUI's `App`, with
  native unit tests (`cargo test -p iroh-share-web`).
- `src/view.rs`: renders the state into one view object per change, including
  status texts, abbreviated links and which actions are enabled.
- `src/net.rs`: pairing, the reconnecting watch session and RPC requests.
- `src/lib.rs`: the wasm bindings, local storage and the URL fragment.

`web/main.js` turns the view into DOM, forwards user intents, and does what only
a page can do: clipboard, opening links, reading dropped ticket files and saving
name archives.

## Differences from the desktop GUI

The browser has no access to the daemon's filesystem or to local paths:

- No folder or file pickers, no folder drops, no Open directory, and no
  "The daemon is on this computer" setting. Paths are typed with Tab completion
  against the daemon.
- No Refresh, which the desktop GUI only offers with the same-filesystem setting.
- Dropping a `.ticket`/`.sendme` file (or ticket text) opens the import row for
  review, as in the desktop GUI.
- Names that follow data are retargeted by choosing a path from a list.

The identity key and saved daemons live in the site's local storage. Clearing
site data loses the identity, and daemons must pair the browser again. Anyone
who can run script on the page's origin can use that identity, so serve it from
an origin you control.

See [UI.md](../iroh-share-proto/UI.md) for the shared frontend rules.
