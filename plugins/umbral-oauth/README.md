# umbral-oauth

OAuth / social-auth for umbral: social login and account connection for Google, GitHub, and more, layered on umbral-auth. Provider tokens are stored encrypted via Masked<T>.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-oauth = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-oauth

## License

Licensed under either of MIT or Apache-2.0, at your option.
