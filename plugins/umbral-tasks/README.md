# umbral-tasks

DB-backed background task queue for umbral. The Celery in Rust shape.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-tasks = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/tasks
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-tasks

## License

Licensed under either of MIT or Apache-2.0, at your option.
