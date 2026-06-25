# Best performance practices

Performance is key in Umbral. Here are some best practices to follow:

1. Generics over dynamic dispatch
2. Inlining critical functions
3. CoW (Clone(Copy)-on-write) smart pointers
4. Rayon and Dashmap to parallelize and share data
