# umbral-cache

Pluggable cache backend for umbral. In-memory, SQLite, and Redis backends, plus view-level cache_page middleware.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```bash
cargo add umbral umbral-cache
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/cache
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-cache

## License

Licensed under either of MIT or Apache-2.0, at your option.
