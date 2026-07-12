## Rust optimizations for core and plugins

1. Borrow instead of clone
2. Ask the map once with entry
3. Parallelize the counting with rayon
4. `swap_remove`when order does not matter
