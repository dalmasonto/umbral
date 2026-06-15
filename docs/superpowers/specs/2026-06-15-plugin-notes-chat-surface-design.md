# Plugin notes → chat surface: inline composer + one-level replies

Date: 2026-06-15
Area: `umbra_website` / `plugin_directory` plugin
Status: design (decisions locked in brainstorming; pending review)

## Problem

The community-notes thread on `/plugins/{slug}` is a flat list of top-level notes posted through a modal `<dialog>`. We want it to read like a chat surface: post inline (no dialog), reply to a specific note, and keep the layout compact. The data model already carries `PluginComment.parent` (`Option<ForeignKey<PluginComment>>`, `on_delete = set_null`, server-managed) for threading; nothing in the UI or the queries uses it yet. This builds on `2026-06-14-live-plugin-notes-design.md` (publish-then-moderate + instant SSE/AJAX insert).

## Decisions (locked in brainstorming)

- **One-level replies (depth 1).** A top-level note has `parent = None`; a reply has `parent = <note id>`. Replies do not nest further — a reply-to-a-reply is rejected server-side (the parent must itself be top-level). This maps exactly onto the existing `parent` FK.
- **Inline composer replaces the dialog.** A chat-style composer sits at the bottom of the thread for new top-level notes. Each top-level note has a "Reply" affordance that reveals an inline reply box in place. The `<dialog>` is removed.
- **Replies are lightweight.** A reply is just a markdown body + optional name. The richer fields (kind, plugin version, backend) stay on the top-level composer only — this keeps replies chat-like and the layout compact.
- **Same safety + moderation as notes.** Replies are publish-then-moderate (created `Visible`; an admin sets `Hidden` later), pass the same hidden honeypot + per-browser submit-interval cookie, and their markdown body is sanitized server-side by the `| markdown` filter (the XSS boundary, now with syntax highlighting).
- **Live via SSE.** A posted reply broadcasts `{ id, html, parent_id }` to `public:plugin-{id}`; the client inserts it into that parent's replies container (deduped by id). Top-level notes broadcast `parent_id: null` and append to the thread, as today.
- **Compact layout.** Tighter row spacing, smaller avatars, replies indented under their parent with a connecting rail. Prose is already reduced to 14px.

## Server changes (`plugin_directory/src/lib.rs`)

1. **`create_note`** gains an optional `parent: Option<i64>`. When set, it loads the parent comment and validates it: the parent must exist, be `Visible`, belong to the same plugin, and be top-level (`parent IS NULL`) — otherwise return `Ok(None)` (treated as a 404/!) so a reply-to-reply or cross-plugin parent can't be forged. The new comment's `parent` FK is set accordingly. The returned `NotePayload` carries `parent_id: Option<i64>`.
2. **`post_plugin_note`** reads an optional `parent_id` form field (parse to `i64`, ignore if absent/blank) and passes it to `create_note`. Honeypot + interval guards unchanged. JSON path returns `{ ok, id, html, parent_id }`; the no-JS redirect path is unchanged (the redirected page shows the reply in its thread).
3. **`render_detail_with`** stops fetching a flat list. Instead: fetch top-level visible notes (`parent IS NULL`, `pinned DESC`, `created_at ASC`, `limit 10` as today), collect their ids, then fetch all visible replies in one batched query (`parent IN (<ids>)`, `created_at ASC`) and group them in memory under their parent — no N+1. Build a `Vec<CommentThread>` where `CommentThread { note: CommentPreview, replies: Vec<CommentPreview> }`.
4. **`NotePayload`** gains `parent_id: Option<i64>`. The SSE broadcast in `create_note` includes it.
5. **Reply route: reuse `POST /plugins/{slug}/notes`** with the `parent_id` field — no new route. `render_comment_row` gains a sibling `render_reply_row` that renders the slim `_reply.html` partial, so a live-inserted reply is byte-identical to a reloaded one (the same parity rule the note row already follows).
6. **`CommentThread`** is a new view-model; `PluginDetail.comments: Vec<CommentPreview>` becomes `Vec<CommentThread>`.

## Template changes

- **`plugin.html`**: remove the `<dialog>`. Add an inline top-level composer at the bottom of the thread (body + kind + name + honeypot + `csrf_input`). The thread loop iterates `CommentThread`s: each renders the note row, its replies (indented, in a `[data-replies-for="<id>"]` container), and a "Reply" button that toggles an inline reply form (body + name + hidden `parent_id` + honeypot + csrf). Tighten spacing for the compact look. Update the success copy.
- **`_comment.html`**: stays the top-level note row, plus the replies container + reply affordance hooks (`data-comment-id` already present). A new **`_reply.html`** partial renders the slim reply row (small avatar, name · date, markdown body, `data-comment-id`, `data-reply` marker).
- **JS** (rewrite the note script): `insertNote({ html, id, parent_id })` — when `parent_id` is set, dedupe by id and append into `[data-replies-for="<parent_id>"]`; else append to the top-level list and bump the counters. The top-level composer and each reply form submit via `fetch` as `application/x-www-form-urlencoded` (the CSRF-safe encoding from the earlier fix), insert in place, then reset. The SSE `note` handler reads `parent_id` and routes the insert the same way. Reply toggles are delegated (so SSE-inserted notes also get a working reply box).

## Tests (`plugin_directory/tests/render_pages.rs`)

- `create_note` with a valid `parent` creates a `Visible` reply whose `parent` points at the note; payload `parent_id` is set.
- `create_note` rejects a parent that is hidden, on another plugin, or itself a reply (returns `None`) — the depth-1 / same-plugin guard.
- `render_detail_with` groups replies under their notes in one batched query (assert the object graph: a note with its replies, ordered).
- A reply rides SSE carrying `parent_id` (the payload shape).
- Top-level notes still post and render unchanged (regression).

## Out of scope

- Deeper nesting (reply-to-reply), reply edit/delete by author, reactions/voting, @mentions, and pagination of replies within a note. All deferred.
- Account-gated posting (still anonymous-with-name, per the existing notes design).
