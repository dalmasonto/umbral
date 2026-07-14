# Page Cache Buffers Eligible Responses Without Size Cap

Category: Performance, Reliability
Severity: Medium

## Finding

The page cache buffers eligible responses in memory before caching them. It has careful bypass logic for cookies, auth, private cache controls, and `Vary`, but does not appear to enforce a maximum object size.

## Evidence

- `plugins/umbral-cache/src/cache_page.rs` buffers cacheable response bodies before storage.
- The bypass rules cover session cookies, authorization headers, `Set-Cookie`, and private/no-store/no-cache responses.
- No cache-object size cap was found in the page cache layer.

## Risk

A large cacheable 200 response can consume substantial memory and be stored in cache. Multiple concurrent large responses can pressure memory and Redis.

## Recommendation

Add a configurable maximum cacheable response size:

- Bypass caching when `Content-Length` exceeds the limit.
- Stop buffering and pass through once the streaming body exceeds the limit.
- Emit metrics for skipped oversized responses.

## Suggested Tests

- Small cacheable response is cached.
- Response larger than the configured limit is served but not cached.
- Streaming response that crosses the limit does not allocate unbounded memory.

