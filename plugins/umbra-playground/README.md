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
3. You should see the 3-pane shell with a left endpoint tree, a center URL strip, and an empty right pane.
4. The left pane should show a "Loading spec..." state for a moment, then a list of endpoints grouped by tag.
5. Click an endpoint; the center URL strip should populate with the method and path.
