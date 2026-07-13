# umbral-graphql

A real GraphQL API derived from your umbral models — relations and all.

This is an optional plugin for umbral, a batteries-included web framework for Rust.

## Install

```toml
[dependencies]
umbral = "0.0.8"
umbral-graphql = "0.0.8"
```

## Use

Nothing is exposed until you say so, and a model you expose is readable but not writable until you say so again:

```rust
GraphqlPlugin::new()
    .expose("post")
    .expose("auth_user")
    .hide("auth_user", "email")   // exposing a model exposes EVERY column of it
    .mutable("post")              // now createPost / updatePost / deletePost exist
```

```graphql
{ post(id: "1") { title author { username } comments { body } } }
```

The double opt-in is deliberate. A read you got wrong leaks data; a write you got wrong destroys it. And unlike REST — where the endpoint returns the shape *you* designed — a GraphQL endpoint returns the shape the *caller* designed, so `expose` deserves more care, not less.

Relations are resolved through a per-request DataLoader, so `posts { author { .. } }` does not become N+1.

## Documentation

- Guide: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/graphql
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral-graphql

## License

Licensed under either of MIT or Apache-2.0, at your option.
