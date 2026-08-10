## Rust optimizations for core and plugins

1. Borrow instead of clone
2. Ask the map once with entry
3. Parallelize the counting with rayon
4. `swap_remove`when order does not matter

## 5 Unsafe Optimizations

1. Skip the bounds check

```rust
fn sum_at(values: &[f64], idx: &[usize]) -> f64 {
    let mut total = 0.0;
    for &i in idx {
        total += unsafe { *values.get_unchecked(i) }; // no bounds check -> we know for sure i is there anyways
    }
    total
}
```

We can use this where we have for loops in the code ie when reading data from the db for something

2. Reserve memory without initialization
3. Pointers across threads
4. Deterministic iterator lengths
5. Reinterpret bytes as a new type
