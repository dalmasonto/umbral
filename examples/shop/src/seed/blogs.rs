//! Editorial seed — tags, 20 long-form posts via `heavy_post`,
//! and three canned comments per post (admin + two demo users +
//! one guest). Comments reference user ids 2 and 3, so the
//! demo-data seed must create alice/bob/carol BEFORE this runs.
//! See `super::all()` for the order.

use content::models::{Category, Comment, Post, PostStatus, Tag};
use umbra::prelude::*;
use umbra_auth::AuthUser;

pub async fn blogs() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if Tag::objects().count().await? == 0 {
        Tag::objects()
            .bulk_create(vec![
                Tag { id: 0, name: "Rust".into(), slug: "rust".into() },
                Tag { id: 0, name: "Web Development".into(), slug: "web-development".into() },
                Tag { id: 0, name: "Database".into(), slug: "database".into() },
                Tag { id: 0, name: "Performance".into(), slug: "performance".into() },
                Tag { id: 0, name: "Architecture".into(), slug: "architecture".into() },
                Tag { id: 0, name: "API Design".into(), slug: "api-design".into() },
                Tag { id: 0, name: "Security".into(), slug: "security".into() },
                Tag { id: 0, name: "DevOps".into(), slug: "devops".into() },
            ])
            .await?;
    }

    if Post::objects().count().await? == 0 {
        let now = chrono::Utc::now();
        let author = ForeignKey::new(1); // shopadmin
        let category = ForeignKey::new(1); // gadgets

        let posts: Vec<Post> = [
            (1, "building-high-performance-apis-in-rust", "Building High-Performance APIs in Rust"),
            (2, "understanding-database-indexes", "Understanding Database Indexes: A Deep Dive"),
            (3, "async-rust-patterns", "Async Rust Patterns for Production Systems"),
            (4, "zero-copy-serialization", "Zero-Copy Serialization Techniques"),
            (5, "web-framework-design-principles", "Web Framework Design Principles"),
            (6, "memory-management-in-systems-programming", "Memory Management in Systems Programming"),
            (7, "scaling-sqlite-to-millions-of-rows", "Scaling SQLite to Millions of Rows"),
            (8, "postgres-query-optimization", "PostgreSQL Query Optimization Strategies"),
            (9, "rust-ownership-model-explained", "The Rust Ownership Model Explained"),
            (10, "building-django-like-frameworks", "Building Django-Like Frameworks in Rust"),
            (11, "concurrency-patterns-in-modern-web-servers", "Concurrency Patterns in Modern Web Servers"),
            (12, "type-safe-database-migrations", "Type-Safe Database Migrations"),
            (13, "rest-api-versioning-strategies", "REST API Versioning Strategies"),
            (14, "caching-strategies-for-web-applications", "Caching Strategies for Web Applications"),
            (15, "securing-web-applications-owasp-top-ten", "Securing Web Applications: OWASP Top Ten"),
            (16, "microservices-vs-monoliths", "Microservices vs Monoliths: A Practical Guide"),
            (17, "real-time-data-processing", "Real-Time Data Processing with Rust"),
            (18, "error-handling-in-distributed-systems", "Error Handling in Distributed Systems"),
            (19, "load-testing-and-benchmarking", "Load Testing and Benchmarking Web APIs"),
            (20, "future-of-web-development", "The Future of Web Development: 2026 and Beyond"),
        ]
        .iter()
        .map(|(idx, slug, title)| heavy_post(*idx, slug, title, &author, &category, &now))
        .collect();

        Post::objects().bulk_create(posts).await?;

        // Three canned comments per post on the first 10.
        let comment_posts = Post::objects().limit(10).fetch().await?;
        let mut comments = vec![];
        for post in &comment_posts {
            comments.push(Comment {
                id: 0,
                post: ForeignKey::new(post.id),
                parent: None,
                author: Some(ForeignKey::new(2)),
                author_name: None,
                author_email: None,
                body: "This is exactly what I was looking for. The explanation of the core concepts really helped clarify things for our team. We ended up adopting several of the patterns described here and saw immediate improvements in our codebase.".into(),
                is_approved: true,
                created_at: now,
            });
            comments.push(Comment {
                id: 0,
                post: ForeignKey::new(post.id),
                parent: None,
                author: Some(ForeignKey::new(3)),
                author_name: None,
                author_email: None,
                body: "Great article! I would love to see a follow-up covering the edge cases we ran into when implementing this at scale. Specifically, how do you handle backpressure when the system is under heavy load?".into(),
                is_approved: true,
                created_at: now,
            });
            comments.push(Comment {
                id: 0,
                post: ForeignKey::new(post.id),
                parent: None,
                author: None,
                author_name: Some("Guest Reader".into()),
                author_email: Some("guest@example.com".into()),
                body: "Thanks for writing this. It saved me hours of research. The benchmarks comparing different approaches were particularly valuable.".into(),
                is_approved: true,
                created_at: now,
            });
        }
        if !comments.is_empty() {
            Comment::objects().bulk_create(comments).await?;
        }
    }

    Ok(())
}

/// Builds a long-form Post body with a consistent shape — same
/// canned sections about engineering trade-offs, just the title
/// substituted in. Demonstrates rich-text content for the admin
/// without us having to author 20 unique articles.
fn heavy_post(
    idx: i64,
    slug: &str,
    title: &str,
    author: &ForeignKey<AuthUser>,
    category: &ForeignKey<Category>,
    now: &chrono::DateTime<chrono::Utc>,
) -> Post {
    let body = format!(
        r#"<h2>Introduction</h2>
<p>Welcome to our comprehensive guide on {}. In this article, we will explore the fundamental concepts, practical implementations, and advanced techniques that have emerged from years of production experience. Whether you are a seasoned developer or just getting started, there is something here for everyone.</p>

<h2>The Problem Space</h2>
<p>When building modern web applications, developers face a unique set of challenges. Systems must handle thousands of concurrent connections while maintaining sub-millisecond response times. Data consistency across distributed nodes becomes non-trivial. Security threats evolve faster than most teams can patch. And perhaps most importantly, the codebase must remain maintainable as the team grows.</p>

<p>Consider a typical e-commerce platform. On any given day, it processes hundreds of thousands of transactions, each touching inventory, payment, shipping, and notification subsystems. A failure in any one component cascades rapidly. The database layer must absorb massive read spikes during flash sales. The API layer must validate every request against sophisticated fraud rules. And all of this must happen while the site remains responsive to browsing customers.</p>

<h2>Core Concepts</h2>
<p>At the heart of any robust solution lies a set of well-understood primitives. First, <strong>ownership and borrowing</strong> ensure that memory is managed without garbage collection pauses. Second, <strong>zero-cost abstractions</strong> let us express high-level intent without runtime overhead. Third, <strong>composability</strong> means we can build complex systems from simple, tested components.</p>

<p>Let us look at a concrete example. Imagine we need to process a stream of incoming orders. In a traditional approach, we might spin up a thread per connection. At low volumes this works fine. But as concurrency increases, context switching dominates CPU time and memory pressure mounts. The modern approach uses asynchronous I/O with a small pool of executor threads. Each connection is represented as a lightweight future, scheduled cooperatively rather than preemptively.</p>

<h2>Implementation Strategies</h2>
<p>There are three primary strategies we have seen succeed in production:</p>
<ol>
<li><strong>Connection pooling:</strong> Reuse expensive TCP and TLS handshakes across requests. A well-tuned pool keeps a small warm set of connections ready, scaling up under load and down during idle periods.</li>
<li><strong>Request batching:</strong> Group small reads into larger multi-row queries. This reduces round-trips and lets the database optimizer do more effective work per query.</li>
<li><strong>Circuit breakers:</strong> When a downstream service fails, stop sending it traffic temporarily. This prevents cascading failures and gives the dependency time to recover.</li>
</ol>

<p>Implementing these requires careful attention to observability. Every pool, batch, and breaker should emit metrics: queue depth, latency percentiles, error rates. Without visibility, tuning becomes guesswork.</p>

<h2>Benchmarks and Results</h2>
<p>We ran a series of benchmarks on a standard cloud instance (4 vCPU, 16 GB RAM). The baseline, unoptimized service handled 2,400 requests per second at p99 latency of 45ms. After applying the strategies above, throughput increased to 18,000 requests per second with p99 latency of 8ms. That is a 7.5x throughput improvement and a 5.6x latency reduction.</p>

<p>Breaking down the wins: connection pooling contributed roughly 2x, batching another 2.5x, and circuit breakers prevented the occasional latency spike that previously dragged the p99 up. The remaining improvement came from removing unnecessary serialization round-trips.</p>

<h2>Common Pitfalls</h2>
<p>Even experienced teams stumble. The most common mistake is over-optimizing too early. We have seen teams implement elaborate caching layers before they understand their actual access patterns. The result is complex invalidation logic, stale data bugs, and minimal performance gain because the cache hit rate is low.</p>

<p>Another frequent error is ignoring backpressure. When the system cannot keep up, it should shed load gracefully rather than queue indefinitely. Unbounded queues masquerade as healthy systems right up until they exhaust memory and crash.</p>

<h2>Future Directions</h2>
<p>The ecosystem continues to evolve. WebAssembly is opening new deployment models. Edge computing is pushing logic closer to users. And advances in database internals are making previously impractical query patterns feasible.</p>

<p>We are particularly excited about compile-time checked SQL, which eliminates an entire class of runtime errors. And the growing maturity of async runtimes means we can write straightforward code that compiles to highly efficient state machines.</p>

<h2>Conclusion</h2>
<p>Building high-performance systems is less about raw speed and more about understanding trade-offs. Every optimization has a cost: complexity, maintainability, or both. The best engineers choose their battles carefully, instrument everything, and validate assumptions with real data.</p>

<p>We hope this guide has given you a solid foundation. The techniques described here have served us well across multiple projects and teams. As always, the code is available in our repository. Pull requests and issue reports are welcome.</p>

<p><em>This is post number {} in our technical deep-dive series. If you enjoyed it, consider subscribing to our newsletter for weekly updates on backend engineering, distributed systems, and framework internals.</em></p>"#,
        title.to_lowercase(),
        idx
    );

    let excerpt = format!(
        "A comprehensive guide exploring {}, covering core concepts, implementation strategies, benchmarks, common pitfalls, and future directions for modern web application development.",
        title.to_lowercase()
    );

    Post {
        id: 0,
        slug: slug.into(),
        title: title.into(),
        excerpt: Some(excerpt.clone()),
        body,
        status: PostStatus::Published,
        author: author.clone(),
        category: Some(category.clone()),
        tags: M2M::empty(),
        cover_image: Some(format!("/static/images/post-{}.jpg", idx)),
        attachment: None,
        is_featured: idx <= 5,
        reading_minutes: 12 + (idx as i32 % 8),
        view_count: 1000 + idx * 150,
        seo_title: Some(format!("{} | Umbra Engineering Blog", title)),
        seo_description: Some(excerpt),
        published_at: Some(*now),
        created_at: *now,
        updated_at: *now,
    }
}
