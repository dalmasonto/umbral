# Realtime Redis Broker Uses Unbounded Queue

Category: Performance, Reliability
Severity: Medium

## Finding

The Redis realtime broker uses an unbounded internal channel. If Redis is slow or unavailable while the application continues publishing, memory can grow without a framework-level backpressure point.

## Evidence

- `plugins/umbral-realtime/src/lib.rs:663-664` creates `tokio::sync::mpsc::unbounded_channel()` for broker messages.
- Realtime has useful frame, replay, and connection caps, but the broker handoff itself is not bounded.

## Risk

A Redis outage or slow network can turn publish traffic into unbounded process memory growth. In a multi-tenant or public event source, that can become a denial-of-service path.

## Recommendation

Use bounded broker queues with an explicit overflow policy:

- Backpressure publishers.
- Drop oldest or newest messages for best-effort channels.
- Emit metrics and warnings when the queue is saturated.
- Make queue capacity configurable.

## Suggested Tests

- Simulate a blocked Redis publisher and assert queue capacity is enforced.
- Verify the selected overflow policy is observable through logs or metrics.

