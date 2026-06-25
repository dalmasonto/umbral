# umbral-storage

Unified storage plugin for umbral: merges static-file serving (umbral-static) and media uploads (umbral-media) onto one Storage trait, with an optional S3 backend.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-storage = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-storage

## License

Licensed under either of MIT or Apache-2.0, at your option.
