//! Realistic seed data — the "existing rows" the migrations and the feed
//! read paths run against.
//!
//! Volumes are env-configurable so the same code seeds a laptop demo or the
//! design's target shape:
//!
//! ```bash
//! UMBRAL_BENCH_USERS=50000 UMBRAL_BENCH_POSTS_PER_USER=10 \
//! UMBRAL_BENCH_LIKES_PER_USER=25 cargo run -- serve   # 50k users / 500k posts
//! ```
//!
//! Defaults are modest (2k users / 20k posts) so a first boot is quick; set
//! `UMBRAL_BENCH_USERS=30000` for the 30k-user / 300k-post shape.
//! Everything is generated deterministically (xorshift64, fixed seed) and
//! **resumable per entity**: users/posts top up to the configured volume when
//! short, users with no follows/likes yet are given theirs, and comments are
//! generated for posts that did not exist before this seed. Growing the
//! dataset (2k → 30k users) just means restarting with the bigger env value —
//! existing rows and their activity are kept.

use std::collections::HashSet;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use umbral::prelude::*;
use umbral_auth::{AuthUser, auth_user};

use crate::models::{Comment, Follow, Like, Post, follow, like, post};

pub struct SeedConfig {
    pub users: i64,
    pub posts_per_user: i64,
    pub likes_per_user: i64,
    pub follows_per_user: i64,
    /// A comment appears on a post with this probability (per mille).
    pub comment_prob_permille: u64,
}

impl SeedConfig {
    pub fn from_env() -> Self {
        let get = |k: &str, d: i64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        SeedConfig {
            users: get("UMBRAL_BENCH_USERS", 2000),
            posts_per_user: get("UMBRAL_BENCH_POSTS_PER_USER", 10),
            likes_per_user: get("UMBRAL_BENCH_LIKES_PER_USER", 25),
            follows_per_user: get("UMBRAL_BENCH_FOLLOWS_PER_USER", 8),
            comment_prob_permille: get("UMBRAL_BENCH_COMMENT_PERMILLE", 600) as u64,
        }
    }

    pub fn posts_total(&self) -> i64 {
        self.users * self.posts_per_user
    }
}

/// Deterministic RNG — a bench seed must reproduce the same data across
/// runs, so no system entropy is involved.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: i64) -> i64 {
        if n <= 0 {
            return 0;
        }
        (self.next() % n as u64) as i64
    }
}

/// Placeholder password hash for bench users. They never log in (scenarios
/// authenticate via `x-bench-user`), so hashing 2,000+ users through argon2
/// (~100 ms CPU each) would only make the seed slow for no measurement value.
const PLACEHOLDER_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$YmVuY2hiZW5jaGJlbmNo$MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA";

const WORDS: &[&str] = &[
    "shipping", "rumor", "sunset", "drift", "pixel", "kernel", "harbor", "cobalt", "timber",
    "signal", "meadow", "atlas", "quartz", "ember", "violet", "canyon", "orbit", "linen",
];

fn body(rng: &mut Rng) -> String {
    format!(
        "{} {} {}",
        WORDS[rng.below(WORDS.len() as i64) as usize],
        WORDS[rng.below(WORDS.len() as i64) as usize],
        WORDS[rng.below(WORDS.len() as i64) as usize],
    )
}

pub async fn all() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cfg = SeedConfig::from_env();
    let mut rng = Rng(0x5EED_BEEF_CAFE_0001);
    let now: DateTime<Utc> = Utc::now();

    let user_ids = ensure_users(&cfg, &mut rng, now).await?;
    if user_ids.len() < cfg.users as usize {
        return Err(format!(
            "seeded/expected {} users but found {} — aborting before FK fan-out",
            cfg.users,
            user_ids.len()
        )
        .into());
    }
    let (post_ids, new_post_ids) = ensure_posts(&cfg, &mut rng, now, &user_ids).await?;
    ensure_follows(&cfg, &mut rng, now, &user_ids).await?;
    ensure_likes(&cfg, &mut rng, now, &user_ids, &post_ids).await?;
    ensure_comments(&cfg, &mut rng, now, &user_ids, &new_post_ids).await?;

    // Boot-time statements (seed inserts, id-list fetches) are real SQL but
    // not request traffic — clear the global SQL/telemetry view so the
    // harness starts from a clean slate.
    crate::telemetry::reset_aggregates();

    eprintln!(
        "[bench-seed] done: {} users, {} posts.",
        user_ids.len(),
        post_ids.len()
    );
    Ok(())
}

/// Ensures `bench_user_1..=cfg.users` exist (top-up semantics) and returns
/// every user id in ascending order.
async fn ensure_users(
    cfg: &SeedConfig,
    rng: &mut Rng,
    now: DateTime<Utc>,
) -> Result<Vec<i64>, Box<dyn std::error::Error + Send + Sync>> {
    let existing: HashSet<String> = AuthUser::objects()
        .fetch()
        .await?
        .into_iter()
        .map(|u| u.username)
        .collect();

    let missing = (1..=cfg.users)
        .filter(|i| !existing.contains(&format!("bench_user_{i}")))
        .count();
    if missing == 0 {
        eprintln!("[bench-seed] users present; reusing {} users.", existing.len());
    } else {
        eprintln!("[bench-seed] inserting {missing} users…");
        let mut chunk: Vec<AuthUser> = Vec::with_capacity(500);
        for i in 1..=cfg.users {
            let username = format!("bench_user_{i}");
            if existing.contains(&username) {
                continue;
            }
            chunk.push(AuthUser {
                id: 0,
                username,
                email: format!("bench{i}@example.com"),
                password_hash: PLACEHOLDER_HASH.to_string(),
                is_active: true,
                is_staff: false,
                is_superuser: false,
                date_joined: now - ChronoDuration::days(rng.below(700)),
                last_login: None,
                email_verified_at: None,
            });
            if chunk.len() == 500 {
                AuthUser::objects().bulk_create(std::mem::take(&mut chunk)).await?;
            }
        }
        if !chunk.is_empty() {
            AuthUser::objects().bulk_create(chunk).await?;
        }
    }

    let ids: Vec<i64> = AuthUser::objects()
        .order_by(auth_user::ID.asc())
        .fetch()
        .await?
        .into_iter()
        .map(|u| u.id)
        .collect();
    Ok(ids)
}

/// Ensures the post table holds `cfg.posts_total()` rows (top-up semantics).
/// Returns (every post id ascending, ids created by THIS run).
async fn ensure_posts(
    cfg: &SeedConfig,
    rng: &mut Rng,
    now: DateTime<Utc>,
    user_ids: &[i64],
) -> Result<(Vec<i64>, Vec<i64>), Box<dyn std::error::Error + Send + Sync>> {
    let have = Post::objects().count().await?;
    let old_max = Post::objects()
        .order_by(post::ID.desc())
        .limit(1)
        .first()
        .await?
        .map(|p| p.id)
        .unwrap_or(0);
    let want = cfg.posts_total();
    if have >= want {
        eprintln!("[bench-seed] posts present ({have}); reusing.");
    } else {
        let missing = want - have;
        eprintln!("[bench-seed] inserting {missing} posts (have {have})…");
        let mut chunk: Vec<Post> = Vec::with_capacity(500);
        for n in 0..missing {
            let u = user_ids[(n as usize) % user_ids.len()];
            let age_min = rng.below(60 * 24 * 30);
            chunk.push(Post {
                id: 0,
                author: ForeignKey::new(u),
                body: body(rng),
                created_at: now - ChronoDuration::minutes(age_min),
            });
            if chunk.len() == 500 {
                Post::objects().bulk_create(std::mem::take(&mut chunk)).await?;
            }
        }
        if !chunk.is_empty() {
            Post::objects().bulk_create(chunk).await?;
        }
    }

    let ids: Vec<i64> = Post::objects()
        .order_by(post::ID.asc())
        .fetch()
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    let new_ids: Vec<i64> = ids.iter().copied().filter(|id| *id > old_max).collect();
    Ok((ids, new_ids))
}

/// Per-user top-up: any user with no outgoing follows gets their set. This is
/// what makes growing the dataset (2k → 30k users) give the NEW users real
/// activity instead of empty profiles.
async fn ensure_follows(
    cfg: &SeedConfig,
    rng: &mut Rng,
    now: DateTime<Utc>,
    user_ids: &[i64],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!("[bench-seed] follows: topping up users without any…");
    let mut chunk: Vec<Follow> = Vec::with_capacity(500);
    let mut made = 0usize;
    for (i, u) in user_ids.iter().enumerate() {
        let has_any = Follow::objects()
            .filter(follow::FOLLOWER.eq(*u))
            .limit(1)
            .exists()
            .await?;
        if has_any {
            continue;
        }
        let mut seen: HashSet<i64> = HashSet::new();
        for _ in 0..cfg.follows_per_user {
            let target = user_ids[rng.below(user_ids.len() as i64) as usize];
            if target != *u && seen.insert(target) {
                chunk.push(Follow {
                    id: 0,
                    follower: ForeignKey::new(*u),
                    following: ForeignKey::new(target),
                    created_at: now - ChronoDuration::days(rng.below(400)),
                });
                made += 1;
            }
            if chunk.len() == 500 {
                Follow::objects().bulk_create(std::mem::take(&mut chunk)).await?;
            }
        }
        if i > 0 && i % 5000 == 0 {
            eprintln!("[bench-seed] follows: scanned {i}/{} users…", user_ids.len());
        }
    }
    if !chunk.is_empty() {
        Follow::objects().bulk_create(chunk).await?;
    }
    eprintln!("[bench-seed] follows: {made} inserted.");
    Ok(())
}

/// Per-user top-up, same contract as follows.
async fn ensure_likes(
    cfg: &SeedConfig,
    rng: &mut Rng,
    now: DateTime<Utc>,
    user_ids: &[i64],
    post_ids: &[i64],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!("[bench-seed] likes: topping up users without any…");
    let mut chunk: Vec<Like> = Vec::with_capacity(500);
    let mut made = 0usize;
    for (i, u) in user_ids.iter().enumerate() {
        let has_any = Like::objects()
            .filter(like::USER.eq(*u))
            .limit(1)
            .exists()
            .await?;
        if has_any {
            continue;
        }
        let mut seen: HashSet<i64> = HashSet::new();
        for _ in 0..cfg.likes_per_user {
            let p = post_ids[rng.below(post_ids.len() as i64) as usize];
            if seen.insert(p) {
                chunk.push(Like {
                    id: 0,
                    user: ForeignKey::new(*u),
                    post: ForeignKey::new(p),
                    created_at: now - ChronoDuration::hours(rng.below(24 * 30)),
                });
                made += 1;
            }
            if chunk.len() == 500 {
                Like::objects().bulk_create(std::mem::take(&mut chunk)).await?;
            }
        }
        if i > 0 && i % 5000 == 0 {
            eprintln!("[bench-seed] likes: scanned {i}/{} users…", user_ids.len());
        }
    }
    if !chunk.is_empty() {
        Like::objects().bulk_create(chunk).await?;
    }
    eprintln!("[bench-seed] likes: {made} inserted.");
    Ok(())
}

/// Covers posts created by THIS run (older posts were covered by their own
/// seed generation).
async fn ensure_comments(
    cfg: &SeedConfig,
    rng: &mut Rng,
    now: DateTime<Utc>,
    user_ids: &[i64],
    new_post_ids: &[i64],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if new_post_ids.is_empty() {
        eprintln!("[bench-seed] comments: no new posts; skipping.");
        return Ok(());
    }
    eprintln!(
        "[bench-seed] inserting comments for {} new posts…",
        new_post_ids.len()
    );
    let mut chunk: Vec<Comment> = Vec::with_capacity(500);
    for (i, p) in new_post_ids.iter().enumerate() {
        if i > 0 && i % 50_000 == 0 {
            eprintln!("[bench-seed] comments: {i}/{} posts…", new_post_ids.len());
        }
        let n = if rng.below(1000) < cfg.comment_prob_permille as i64 {
            1 + rng.below(2)
        } else {
            0
        };
        for _ in 0..n {
            chunk.push(Comment {
                id: 0,
                post: ForeignKey::new(*p),
                author: ForeignKey::new(user_ids[rng.below(user_ids.len() as i64) as usize]),
                body: body(rng),
                created_at: now - ChronoDuration::hours(rng.below(24 * 30)),
            });
            if chunk.len() == 500 {
                Comment::objects().bulk_create(std::mem::take(&mut chunk)).await?;
            }
        }
    }
    if !chunk.is_empty() {
        Comment::objects().bulk_create(chunk).await?;
    }
    Ok(())
}
