# umbral-permissions

Role-based access control for umbral: ContentType, Permission, Group, user-group and user-permission M2M tables, plus the has_perm / user_perms query layer.

This is a built-in plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.1"
umbral-permissions = "0.0.1"
```

Register the plugin when you build your app, then use it through the umbral facade. See the documentation for the exact builder call and the settings it exposes.

## Documentation

- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-permissions

## License

Licensed under either of MIT or Apache-2.0, at your option.
