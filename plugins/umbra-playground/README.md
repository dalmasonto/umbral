> **Status:** v1.0 shipped. See `docs/superpowers/specs/2026-06-02-rest-playground-design.md` for the design and `docs/superpowers/plans/2026-06-02-rest-playground.md` for the build order.

# umbra-playground

Interactive API playground UI for umbra-rest. A 3-pane Postman-style UI mounted at `/api/playground/`. Fetches the existing `umbra-openapi` JSON spec at runtime and renders a navigable endpoint tree, request builder, and response viewer.

## Quick start

```rust
use umbra_playground::PlaygroundPlugin;
use umbra_rest::RestPlugin;
use umbra_openapi::OpenApiPlugin;

let app = App::builder()
    .plugin(RestPlugin::default())
    .plugin(OpenApiPlugin::new())
    .plugin(PlaygroundPlugin::new())    // mounts at /api/playground/
    .build();
```

`cargo build` produces the bundled React UI. Requires `esbuild` and `tailwindcss` in `$PATH` (Node 20+). Install with:

```
npm i -g esbuild tailwindcss
```

If either is missing, the plugin still compiles and serves a placeholder page that explains what to install.

## Configuration

```rust
PlaygroundPlugin::new().at("/api/docs/playground")  // mount elsewhere
```

## v1 limitations

- Same-origin only (no CORS proxy)
- Auth is a single bearer-token input
- Body is a JSON textarea (no schema-driven form)
- Request history is localStorage-only (per browser, per device)
- Pane sizes are fixed

See `docs/superpowers/specs/2026-06-02-rest-playground-design.md` for the full design.

## Manual smoke test

After `cargo build -p umbra-playground` with the CLIs installed:

1. Run an example app that registers `RestPlugin`, `OpenApiPlugin`, and `PlaygroundPlugin`.
2. Open `http://localhost:<port>/api/playground/` in a browser.
3. The 3-pane shell renders: left endpoint tree, center request builder, right response viewer.
4. The left pane shows a "Loading spec..." state, then a list of endpoints grouped by tag.
5. Click an endpoint; the center URL strip populates with the method and path.
6. Click "Send"; the right pane shows the response status, body, headers, and a `cURL` tab.
7. Click Send again; the History tab in the right pane shows 2 entries; click an entry to restore.
8. Reload the page; the History tab should still show the entries (loaded from localStorage).
