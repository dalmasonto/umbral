# umbral-analytics

Product-analytics event capture for umbral. PostHog backend with fire-and-forget capture, ambient client, and opt-in per-request middleware.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-analytics = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/analytics
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-analytics

## License

Licensed under either of MIT or Apache-2.0, at your option.
