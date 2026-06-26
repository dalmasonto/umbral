# umbral-realtime

Real-time push for umbral - SSE + WebSocket, with user- and group/room-targeted delivery over a pluggable broker.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-realtime = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/realtime
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-realtime

## License

Licensed under either of MIT or Apache-2.0, at your option.
