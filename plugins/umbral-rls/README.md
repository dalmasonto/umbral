# umbral-rls

Postgres Row-Level Security plugin for umbral. Declares policies once at App::build; the plugin's on_ready hook applies them via ALTER TABLE ENABLE ROW LEVEL SECURITY + CREATE POLICY.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```bash
cargo add umbral umbral-rls
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/rls
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-rls

## License

Licensed under either of MIT or Apache-2.0, at your option.
