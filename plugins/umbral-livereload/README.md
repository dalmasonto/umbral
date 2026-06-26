# umbral-livereload

Opt-in dev live-reload for umbral - a file watcher pushes reload / CSS-swap events to the browser over SSE; the client script is auto-injected into HTML responses. Dev-only, zero per-app code.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-livereload = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/live-reload
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-livereload

## License

Licensed under either of MIT or Apache-2.0, at your option.
