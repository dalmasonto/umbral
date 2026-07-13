# umbral-signals

In-process pub/sub signals for umbral. Plugins emit events; other plugins subscribe.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```bash
cargo add umbral umbral-signals
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/signals
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-signals

## License

Licensed under either of MIT or Apache-2.0, at your option.
