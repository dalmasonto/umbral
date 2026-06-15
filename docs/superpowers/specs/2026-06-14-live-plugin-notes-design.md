# Live plugin notes: publish-then-moderate + instant SSE insert

Date: 2026-06-14
Area: `umbra_website` / `plugin_directory` plugin
Status: approved (design)

## Problem

The plugin detail page (`/plugins/{slug}`) already has a community-notes thread, a `POST /plugins/{slug}/notes` route, and an SSE subscription to `public:plugin-{id}`. But the flow is moderate-**then**-publish:

- `create_note` writes the comment as `moderation = Pending`, so it is invisible until an admin approves.
- The SSE `note` event carries only a banner ("someone posted a note, awaiting review"), never the comment.
- The detail page renders only `Visible` rows.

We want the opposite: a posted note shows immediately for everyone, no moderation gate up front, and an admin can `Hide` it afterward. The page must not reload on submit, and the live note must appear in the thread via SSE.

## Decisions

- **Visibility model: publish-then-moderate.** New notes are created `Visible`. `Hidden` is what an admin sets later to take one down. No new enum states: the count annotation and detail query already key on `"visible"`, so flipping the default is end-to-end consistent.
- **Scope: notes only.** Only `create_note` (community notes) flips to instant-visible. Issue reports (`create_report`) and plugin submissions (`create_submission`) stay `Pending`; they are a moderation queue, not a public thread.
- **New note position: bottom.** Append after existing notes, matching the server's `created_at ASC` ordering (pinned stay on top). Live and reloaded views agree.
- **Spam guard: light.** A hidden honeypot field plus a per-browser minimum submit interval (cookie-based). No new deps. Pairs with framework-level rate limiting later (REAL-GAPS Part B #10).
- **Render parity via a shared partial.** The comment row markup moves to `_comment.html`, used by both the page loop and the server-side render of the SSE/AJAX payload. The `| markdown` filter (pulldown-cmark + ammonia) sanitizes the body on the server, so the broadcast HTML is safe to insert with `innerHTML`. This is the XSS boundary now that bodies are public the instant they are posted.

## Server changes (`plugin_directory/src/lib.rs`)

1. `create_note`: set `moderation = CommentModeration::Visible`. After the ORM write, build a `CommentPreview` for the new row, render `plugin_directory/_comment.html` to an HTML string, and return `Option<NotePayload { id, html }>` (`None` for an unknown slug). Broadcast `{ id, html }` to `public:plugin-{id}` (no-op when realtime is not installed).
2. `post_plugin_note`:
   - Honeypot: if the hidden `website` field is non-empty, accept with `{ "ok": true, "skipped": true }` and write nothing.
   - Interval: read cookie `pd_nt` (last-post epoch secs); if `now - last < 20`, reply `429`; else proceed and `Set-Cookie: pd_nt=<now>`.
   - On the `fetch()` (JSON) path, return `{ "ok": true, "id": N, "html": "<article ...>" }`.
   - No-JS path still does POST/redirect/GET; the note is now `Visible`, so the redirected page shows it. Banner copy drops "awaiting review".
3. `CommentPreview` gains an `id: i64` field (set from `c.id`) so the row carries `data-comment-id`.

## Template changes

- New `_comment.html`: the `<article>` row, with `data-comment-id="{{ comment.id }}"`.
- `plugin.html`: loop body becomes `{% include "plugin_directory/_comment.html" %}`. Add a hidden honeypot input to the dialog form. Add `data-note-count` hooks to the three count spots (stat value, tab badge, "N notes" line). Rewrite the JS:
  - `insertNote(html)`: remove the empty-state placeholder, dedupe by `[data-comment-id]`, append, increment every `data-note-count`.
  - `fetch()` success: parse `{ id, html }`, call `insertNote`, show a "Posted" success line, close + reset the dialog.
  - SSE `note`: parse `{ id, html }`, call `insertNote`. The submitter's own echo is deduped by id. Other open tabs get it live. Drop the old "awaiting review" banner.

## Tests (`plugin_directory/tests/render_pages.rs`)

Invert the note assertions to the new behavior:
- `create_note` returns `Some(payload)`; the posted row is `CommentModeration::Visible`.
- The SSE payload contains the rendered `html` (and the body), not a `pending` flag.
- The note appears in the visible thread without admin action.
- Unknown slug still returns `None`.
- Reports and submissions still assert `Pending` (unchanged).

## Out of scope

Framework rate limiting, account-gated posting, edit/delete of a note by its author. These are follow-ups, not part of this change.
