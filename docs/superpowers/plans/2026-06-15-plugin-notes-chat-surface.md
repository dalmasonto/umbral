# Plugin Notes Chat Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the flat plugin-notes thread into a chat surface: one-level replies on the existing `PluginComment.parent` FK, an inline composer replacing the modal dialog, grouped (no-N+1) rendering, and SSE inserts threaded by `parent_id`.

**Architecture:** Server stays ORM-pure: `create_note` gains a validated optional `parent`; `render_detail_with` fetches all visible comments in one reverse query and partitions them in memory into `CommentThread { note, replies }`. Templates render a slim `_reply.html` under each note; the JS routes AJAX/SSE inserts to the right replies container by `parent_id`.

**Tech Stack:** Rust (umbra ORM), minijinja templates, vanilla JS, umbra-realtime SSE.

**Spec:** `docs/superpowers/specs/2026-06-15-plugin-notes-chat-surface-design.md`

All paths are under `umbra_website/plugins/plugin_directory/`. Cargo runs from `umbra_website/` (a standalone project) — but a dev server is live there, so tests run via the steps below without `cargo clean`. The notes test harness uses in-memory SQLite (`tests/render_pages.rs`).

---

## File Structure

- **`src/lib.rs`** — `create_note` (add `parent`), `NotePayload` (add `parent_id`), `render_detail_with` (partition into threads), `CommentThread` (new view-model), `PluginDetail.comments` (type change), `build()` (grouping), `render_reply_row` (new), `post_plugin_note` (read `parent_id`).
- **`templates/plugin_directory/_comment.html`** — the top-level note row (gains a replies container + reply affordance hooks).
- **`templates/plugin_directory/_reply.html`** — NEW slim reply row partial.
- **`templates/plugin_directory/plugin.html`** — thread loop, inline composer (replaces `<dialog>`), per-note reply form, JS rewrite, compact CSS.
- **`tests/render_pages.rs`** — reply create/validate tests, thread-grouping test, updated existing `create_note` calls.

---

### Task 1: `create_note` accepts a validated `parent`; `NotePayload.parent_id`

**Files:**
- Modify: `src/lib.rs` (`NotePayload` ~1719, `create_note` ~854)
- Modify: `tests/render_pages.rs` (new test; update the two existing `create_note` calls at ~599 and ~650)

- [ ] **Step 1: Write the failing test**

Add to `tests/render_pages.rs` (in the same module as the other note tests; it uses the in-memory DB set up by `boot()`/`seed()`):

```rust
#[tokio::test]
async fn create_note_threads_replies_under_a_visible_top_level_note() {
    boot().await;
    seed().await;

    // A top-level note.
    let note = create_note("umbra-rest", "Parent note body.", "general", None)
        .await
        .expect("note create ok")
        .expect("a payload for an existing plugin");
    assert!(note.parent_id.is_none(), "a top-level note has no parent_id");

    // A reply to it.
    let reply = create_note("umbra-rest", "A reply body.", "general", Some(note.id))
        .await
        .expect("reply create ok")
        .expect("a payload for a valid parent");
    assert_eq!(
        reply.parent_id,
        Some(note.id),
        "the reply payload carries the parent note id"
    );

    // Persisted with parent set + Visible.
    let row = PluginComment::objects()
        .filter(plugin_directory::models::plugin_comment::BODY.eq("A reply body."))
        .first()
        .await
        .expect("query the reply")
        .expect("the reply row exists");
    assert_eq!(row.parent.as_ref().map(|fk| fk.id()), Some(note.id));
    assert_eq!(row.moderation, CommentModeration::Visible);

    // A reply-to-a-reply is rejected (depth-1): parent must be top-level.
    let nested = create_note("umbra-rest", "Nested.", "general", Some(reply.id))
        .await
        .expect("create ok");
    assert!(nested.is_none(), "replying to a reply is rejected (depth-1)");

    // A parent id that doesn't exist is rejected.
    let bad = create_note("umbra-rest", "Orphan.", "general", Some(999_999))
        .await
        .expect("create ok");
    assert!(bad.is_none(), "an unknown parent id is rejected");
}
```

Then update the TWO existing `create_note` calls to pass the new trailing `None` arg:
- ~line 599: `create_note("umbra-rest", "Works great on Postgres 16.", "usage_note", Some("Reviewer".to_string()))` → add `, None` before the closing `)`.
- ~line 650: `create_note("does-not-exist", "body", "general", None)` → `create_note("does-not-exist", "body", "general", None, None)`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd /home/dalmas/E/projects/umbra/umbra_website && cargo test -p plugin_directory --test render_pages create_note_threads_replies_under_a_visible_top_level_note`
Expected: FAIL — compile error (`create_note` takes 4 args, `NotePayload` has no `parent_id`).

- [ ] **Step 3: Add `parent_id` to `NotePayload`**

In `src/lib.rs`, the `NotePayload` struct (~1719):

```rust
#[derive(Debug, Serialize)]
pub struct NotePayload {
    pub id: i64,
    pub html: String,
    /// The note this is a reply to, or `None` for a top-level note. Lets the
    /// client route a live insert into the right replies container.
    pub parent_id: Option<i64>,
}
```

- [ ] **Step 4: Add the `parent` parameter + validation to `create_note`**

Change the signature (add `parent` as the last arg) and insert the validation after the plugin lookup. Replace the existing `create_note` body's tail (from the plugin-resolved point through the `Ok(Some(...))`) so it reads:

```rust
pub async fn create_note(
    slug: &str,
    body: &str,
    kind: &str,
    author_label: Option<String>,
    parent: Option<i64>,
) -> Result<Option<NotePayload>, String> {
    let Some(plugin) = PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // A reply: the parent must be a VISIBLE, top-level (parent IS NULL) comment
    // on THIS plugin. Anything else (unknown id, hidden, cross-plugin, or a
    // reply-to-a-reply) is rejected as a 404 so it can't be forged. Depth stays 1.
    if let Some(parent_id) = parent {
        let ok = match pd::PluginComment::objects()
            .filter(plugin_comment::ID.eq(&parent_id))
            .filter(plugin_comment::MODERATION.eq("visible"))
            .first()
            .await
            .map_err(|e| e.to_string())?
        {
            Some(p) => p.plugin.id() == plugin.id && p.parent.is_none(),
            None => false,
        };
        if !ok {
            return Ok(None);
        }
    }

    let kind = match kind {
        "question" => CommentKind::Question,
        "usage_note" => CommentKind::UsageNote,
        "compatibility_note" => CommentKind::CompatibilityNote,
        "migration_note" => CommentKind::MigrationNote,
        _ => CommentKind::General,
    };

    let mut comment = pd::PluginComment::default();
    comment.plugin = ForeignKey::new(plugin.id);
    comment.parent = parent.map(ForeignKey::new);
    comment.body = body.to_string();
    comment.kind = kind;
    comment.moderation = CommentModeration::Visible;
    comment.author_label = author_label;

    let created = pd::PluginComment::objects()
        .create(comment)
        .await
        .map_err(|e| e.to_string())?;

    let id = created.id;
    let preview = CommentPreview::from_model(created);
    let html = render_comment_row(&preview)?;

    umbra_realtime::Realtime::to_group(format!("public:plugin-{}", plugin.id))
        .send(
            "note",
            &serde_json::json!({ "id": id, "html": html, "parent_id": parent }),
        )
        .await;
    Ok(Some(NotePayload {
        id,
        html,
        parent_id: parent,
    }))
}
```

Also update the ONE caller in `post_plugin_note` (it currently calls `create_note(&slug, body, kind, author_label)`): add a trailing `, None` for now (Task 3 wires the real `parent_id`). Find the `create_note(` call inside `post_plugin_note` (~line 761) and append `, None` as the last argument.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd /home/dalmas/E/projects/umbra/umbra_website && cargo test -p plugin_directory --test render_pages create_note_threads_replies_under_a_visible_top_level_note`
Expected: PASS.

- [ ] **Step 6: Run the existing note test to confirm no regression**

Run: `cargo test -p plugin_directory --test render_pages` (from `umbra_website/`)
Expected: PASS (the updated existing `create_note` calls compile + pass).

- [ ] **Step 7: Commit**

```bash
cd /home/dalmas/E/projects/umbra
cargo fmt --manifest-path umbra_website/Cargo.toml
git add umbra_website/plugins/plugin_directory/src/lib.rs umbra_website/plugins/plugin_directory/tests/render_pages.rs
git commit -m "feat(notes): create_note accepts a validated parent (depth-1 replies)"
```

---

### Task 2: Group replies into `CommentThread` + render them nested

**Files:**
- Modify: `src/lib.rs` (`CommentThread` new, `render_detail_with` ~1307, `PluginDetail.comments` ~1378, `build()` ~1441/1525, `render_reply_row` new, `create_note` reply-rendering)
- Create: `templates/plugin_directory/_reply.html`
- Modify: `templates/plugin_directory/_comment.html` (replies container + reply hook), `templates/plugin_directory/plugin.html` (thread loop)
- Modify: `tests/render_pages.rs` (thread-grouping test)

- [ ] **Step 1: Write the failing test**

Add to `tests/render_pages.rs`:

```rust
#[tokio::test]
async fn detail_page_nests_replies_under_their_note() {
    boot().await;
    seed().await;

    let note = create_note("umbra-rest", "Top note here.", "general", None)
        .await
        .expect("ok")
        .expect("payload");
    create_note("umbra-rest", "First reply.", "general", Some(note.id))
        .await
        .expect("ok")
        .expect("payload");

    let html = render_detail("umbra-rest")
        .await
        .expect("render ok")
        .expect("the plugin exists");

    // Both render, and the reply row carries the slim `data-reply` marker so
    // it's visually a reply, nested in the parent's replies container.
    assert!(html.contains("Top note here."), "note renders");
    assert!(html.contains("First reply."), "reply renders");
    assert!(html.contains("data-reply"), "reply uses the slim reply partial");
    assert!(
        html.contains(&format!("data-replies-for=\"{}\"", note.id)),
        "the note has a replies container keyed by its id"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd /home/dalmas/E/projects/umbra/umbra_website && cargo test -p plugin_directory --test render_pages detail_page_nests_replies_under_their_note`
Expected: FAIL (no `data-reply` / `data-replies-for` in output; replies render flat).

- [ ] **Step 3: Create the slim reply partial**

Create `templates/plugin_directory/_reply.html`:

```html
{# One reply row — slimmer than a top-level note (_comment.html). Rendered by
   the page loop and, identically, server-side for the live SSE / AJAX insert.
   `comment` is a `CommentPreview`. Body sanitized by `| markdown` (the XSS
   boundary). `data-reply` marks it slim; `data-comment-id` dedupes live. #}
<article class="pd-reply" data-comment-id="{{ comment.id }}" data-reply>
  <span class="pd-reply-avatar">{{ comment.initials }}</span>
  <div class="min-w-0">
    <div class="pd-reply-head"><span class="pd-reply-name">{{ comment.name }}</span> · {{ comment.created }}</div>
    <div class="pd-prose text-[13.5px]" data-md>{{ comment.body | markdown }}</div>
  </div>
</article>
```

- [ ] **Step 4: Add `CommentThread`, `render_reply_row`, and partition the query**

In `src/lib.rs`:

(a) Add the view-model near `CommentPreview` (after its `impl`, ~line 1703):

```rust
/// One top-level note plus its (depth-1) replies, in render order.
#[derive(Debug, Serialize)]
struct CommentThread {
    note: CommentPreview,
    replies: Vec<CommentPreview>,
}
```

(b) Add a reply renderer next to `render_comment_row` (~line 1709):

```rust
/// Render one reply row via the slim `_reply.html`, the reply counterpart to
/// [`render_comment_row`] — so a live-inserted reply is byte-identical to a
/// reloaded one. Body sanitized by `| markdown`, safe to broadcast.
fn render_reply_row(preview: &CommentPreview) -> Result<String, String> {
    umbra::templates::render(
        "plugin_directory/_reply.html",
        &umbra::templates::context! { comment => preview },
    )
    .map_err(|e| e.to_string())
}
```

(c) In `create_note`, render replies with the slim partial. Replace the `let html = render_comment_row(&preview)?;` line with:

```rust
    let html = if parent.is_some() {
        render_reply_row(&preview)?
    } else {
        render_comment_row(&preview)?
    };
```

(d) Replace the comment fetch in `render_detail_with` (~1307-1316) — drop `.limit(10)` and partition in memory:

```rust
    // All visible comments for this plugin in one reverse query, ordered so
    // pinned top-level notes lead and everything is chronological. Partition
    // in memory (ORM-pure: read the hydrated `parent` Option) into top-level
    // notes (first 10) and their replies — no N+1, no IS NULL/IN predicate.
    let all_comments = plugin
        .reverse::<pd::PluginComment>()
        .map_err(|e| e.to_string())?
        .filter(plugin_comment::MODERATION.eq("visible"))
        .order_by(plugin_comment::PINNED.desc())
        .order_by(plugin_comment::CREATED_AT.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    let mut replies_by_parent: std::collections::HashMap<i64, Vec<pd::PluginComment>> =
        std::collections::HashMap::new();
    let mut top_level: Vec<pd::PluginComment> = Vec::new();
    for c in all_comments {
        match c.parent.as_ref().map(|fk| fk.id()) {
            Some(pid) => replies_by_parent.entry(pid).or_default().push(c),
            None => top_level.push(c),
        }
    }
    top_level.truncate(10);
    let comment_rows: Vec<CommentThread> = top_level
        .into_iter()
        .map(|note| {
            let replies = replies_by_parent
                .remove(&note.id)
                .unwrap_or_default()
                .into_iter()
                .map(CommentPreview::from_model)
                .collect();
            CommentThread {
                note: CommentPreview::from_model(note),
                replies,
            }
        })
        .collect();
```

(e) Change `PluginDetail.comments` type (~1378) from `comments: Vec<CommentPreview>,` to `comments: Vec<CommentThread>,`.

(f) In `build()`, replace the `comment_previews` mapping (~1525-1529) so it consumes the threads and counts notes + replies:

```rust
        let notes = comments
            .iter()
            .map(|t| 1 + t.replies.len() as i64)
            .sum::<i64>();
        let comment_previews = comments;
```

And change `build()`'s parameter type (~1441) from `comments: Vec<pd::PluginComment>,` to `comments: Vec<CommentThread>,`. (The `comments: comment_previews` field assignment at ~1601 stays.)

- [ ] **Step 5: Update the thread loop in `plugin.html`**

Leave `_comment.html` UNCHANGED (it stays a single note row so `render_comment_row` keeps producing byte-identical output). Only change the notes loop in `plugin.html` — the `{% for comment in plugin.comments %}` block (~line 421) — to iterate threads, alias the note as `comment` for the include, then render the replies container right after:

```html
              {% for thread in plugin.comments %}
              {% set comment = thread.note %}
              {% include "plugin_directory/_comment.html" %}
              <div class="pd-replies" data-replies-for="{{ thread.note.id }}">
                {% for comment in thread.replies %}
                {% include "plugin_directory/_reply.html" %}
                {% endfor %}
              </div>
              {% else %}
              <p class="m-0 rounded-[14px] border border-hairline bg-paper px-4 py-5 text-[14px] text-muted" data-note-empty>No notes yet. Be the first to share how this plugin works for you.</p>
              {% endfor %}
```

`{% include %}` re-uses the surrounding `comment` variable, so aliasing `thread.note` → `comment` keeps `_comment.html` working untouched; the inner `{% for comment in thread.replies %}` re-binds `comment` to each reply for `_reply.html`. The reply toggle button + inline reply form come in Task 3.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd /home/dalmas/E/projects/umbra/umbra_website && cargo test -p plugin_directory --test render_pages detail_page_nests_replies_under_their_note`
Expected: PASS.

- [ ] **Step 7: Run the full render_pages suite (no regression)**

Run: `cargo test -p plugin_directory --test render_pages` (from `umbra_website/`)
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
cd /home/dalmas/E/projects/umbra
cargo fmt --manifest-path umbra_website/Cargo.toml
git add umbra_website/plugins/plugin_directory/src/lib.rs \
  umbra_website/plugins/plugin_directory/templates/plugin_directory/_reply.html \
  umbra_website/plugins/plugin_directory/templates/plugin_directory/plugin.html \
  umbra_website/plugins/plugin_directory/tests/render_pages.rs
git commit -m "feat(notes): group replies into threads and render them nested"
```

---

### Task 3: `post_plugin_note` reads `parent_id`; inline composer + reply UI + JS

These are route + template + JS changes verified against the live dev server (static/template, no rebuild for the template; lib.rs change rebuilds via cargo-watch). No JS unit harness, so verification is "served + live behavior".

**Files:**
- Modify: `src/lib.rs` (`post_plugin_note` ~719)
- Modify: `templates/plugin_directory/plugin.html` (remove `<dialog>`, add inline composer + per-note reply form, rewrite JS)

- [ ] **Step 1: `post_plugin_note` reads `parent_id`**

In `src/lib.rs`, inside `post_plugin_note`, after `author_label` is computed and before the `create_note(...)` call, parse the optional `parent_id`:

```rust
    let parent_id = form
        .get("parent_id")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
```

Then change the `create_note(&slug, body, kind, author_label, None)` call (the `None` added in Task 1) to pass `parent_id`:

```rust
    let Some(payload) = create_note(&slug, body, kind, author_label, parent_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    else {
```

And include `parent_id` in the JSON response body. Find the `serde_json::json!({ "ok": true, "id": payload.id, "html": payload.html, })` block and add `"parent_id": payload.parent_id,`.

- [ ] **Step 2: Replace the dialog with an inline composer (plugin.html)**

Remove the `<dialog class="pd-dialog" id="pd-note-dialog"> … </dialog>` block (~line 507-552). Replace the "Add a note" button (~line 414, `<button ... data-open-note>Add a note</button>`) and the dialog with an inline composer placed at the **bottom** of the notes panel, right after the `#pd-note-list` div (~line 426). Insert:

```html
        <form class="pd-composer" method="post" action="/plugins/{{ plugin.slug }}/notes" data-note-form>
          <div class="hidden" aria-hidden="true"><label>Website<input type="text" name="website" tabindex="-1" autocomplete="off"></label></div>
          {{ csrf_input }}
          <textarea class="pd-field" name="body" required rows="2" placeholder="Add a note — Markdown supported, code gets highlighted."></textarea>
          <div class="pd-composer-row">
            <select class="pd-field pd-composer-kind" name="kind">
              {% for opt in plugin.note_kinds %}<option value="{{ opt.value }}">{{ opt.label }}</option>{% endfor %}
            </select>
            <input class="pd-field pd-composer-name" name="author_label" maxlength="120" placeholder="Name (optional)">
            <button type="submit" class="btn-accent">Post</button>
          </div>
        </form>
```

Remove the now-unused `data-open-note`/`data-close-note` handlers from the JS (Step 4 rewrite covers this).

- [ ] **Step 3: Add the per-note reply toggle + inline reply form**

In `plugin.html`, in the thread loop (from Task 2), after the `.pd-replies` container, add a reply toggle + a hidden inline reply form per note:

```html
              <button type="button" class="pd-reply-toggle" data-reply-toggle="{{ thread.note.id }}">Reply</button>
              <form class="pd-reply-form hidden" method="post" action="/plugins/{{ plugin.slug }}/notes" data-reply-form="{{ thread.note.id }}">
                <div class="hidden" aria-hidden="true"><label>Website<input type="text" name="website" tabindex="-1" autocomplete="off"></label></div>
                {{ csrf_input }}
                <input type="hidden" name="parent_id" value="{{ thread.note.id }}">
                <textarea class="pd-field" name="body" required rows="1" placeholder="Reply… Markdown supported."></textarea>
                <div class="pd-composer-row">
                  <input class="pd-field pd-composer-name" name="author_label" maxlength="120" placeholder="Name (optional)">
                  <button type="submit" class="btn-accent">Reply</button>
                </div>
              </form>
```

- [ ] **Step 4: Rewrite the note JS**

Replace the note-dialog IIFE and `insertNote`/SSE blocks in `plugin.html`'s `<script>` with this (keeps the tabs/copy code above it untouched):

```html
<script>
  (function () {
    // Submit any note/reply form via fetch as urlencoded (NOT multipart — the
    // CSRF middleware + Form extractor only accept urlencoded; multipart 403/415s).
    function wireForm(form) {
      if (!(form && window.fetch && window.FormData)) return;
      form.addEventListener('submit', function (e) {
        e.preventDefault();
        var btn = form.querySelector('button[type="submit"]');
        if (btn) btn.disabled = true;
        fetch(form.action, {
          method: 'POST',
          headers: { 'Accept': 'application/json' },
          body: new URLSearchParams(new FormData(form))
        }).then(function (res) {
          if (!res.ok) throw new Error('post failed: ' + res.status);
          return res.json();
        }).then(function (d) {
          if (d && d.html) insertNote(d);
          form.reset();
          if (form.classList.contains('pd-reply-form')) form.classList.add('hidden');
        }).catch(function () { form.submit(); })
          .finally(function () { if (btn) btn.disabled = false; });
      });
    }
    document.querySelectorAll('[data-note-form], [data-reply-form]').forEach(wireForm);

    // Reply toggles (delegated so SSE-inserted notes work too).
    document.addEventListener('click', function (e) {
      var t = e.target.closest('[data-reply-toggle]');
      if (!t) return;
      var id = t.getAttribute('data-reply-toggle');
      var f = document.querySelector('[data-reply-form="' + id + '"]');
      if (f) { f.classList.toggle('hidden'); var ta = f.querySelector('textarea'); if (ta && !f.classList.contains('hidden')) ta.focus(); }
    });
  })();

  // Insert a rendered note or reply. `d` = { id, html, parent_id }. A reply
  // (parent_id set) appends into that note's [data-replies-for]; a top-level
  // note appends to #pd-note-list and bumps the counters. Deduped by id.
  function insertNote(d) {
    var html = d.html, id = String(d.id);
    var tmp = document.createElement('div');
    tmp.innerHTML = html.trim();
    var row = tmp.firstElementChild;
    if (!row) return;
    if (document.querySelector('[data-comment-id="' + id + '"]')) return; // dedupe
    if (d.parent_id) {
      var box = document.querySelector('[data-replies-for="' + d.parent_id + '"]');
      if (box) box.appendChild(row);
      return;
    }
    var list = document.getElementById('pd-note-list');
    if (!list) return;
    var empty = list.querySelector('[data-note-empty]');
    if (empty) empty.remove();
    list.appendChild(row);
    bumpNoteCount();
  }

  function bumpNoteCount() {
    var next = null;
    document.querySelectorAll('[data-note-count]').forEach(function (el) {
      var n = parseInt(el.textContent.replace(/[^0-9]/g, ''), 10);
      next = (isNaN(n) ? 0 : n) + 1;
      el.textContent = String(next);
    });
    var plural = document.querySelector('[data-note-plural]');
    if (plural && next !== null) plural.textContent = next === 1 ? '' : 's';
  }

  (function () {
    if (!('EventSource' in window)) return;
    var es = new EventSource('/realtime/sse?groups=public:plugin-{{ plugin.id }}');
    es.addEventListener('note', function (e) {
      try { var d = JSON.parse(e.data); if (d && d.html) insertNote(d); } catch (_) {}
    });
  })();
</script>
```

(Note: the replies container in the page loop only exists for notes present at load. An SSE reply whose parent note isn't on the page is dropped by the `if (box)` guard — acceptable; it appears on next reload.)

- [ ] **Step 5: Let cargo-watch rebuild (lib.rs changed), then verify live**

After the lib.rs edit, the dev server rebuilds. Once `http://localhost:8100` returns 200 again:

```bash
# top-level note still posts via AJAX (urlencoded) → 200 JSON with parent_id null
curl -s http://localhost:8100/plugins/umbra-admin > /tmp/p.html
grep -c 'data-note-form\|pd-composer' /tmp/p.html     # composer present (>0)
grep -c 'pd-dialog' /tmp/p.html                       # dialog gone (0)
grep -c 'data-reply-toggle\|data-replies-for' /tmp/p.html  # reply UI present (>0)
```

Then post a reply through the route (reuse the CSRF flow from the earlier fix: fetch the page, extract `csrf_token`, send urlencoded with `parent_id`) and confirm a 200 JSON with `"parent_id"` set. Visually confirm in the browser: posting a note appends inline without reload; "Reply" reveals a box; a reply nests under the note and live-updates in a second tab.

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbra
cargo fmt --manifest-path umbra_website/Cargo.toml
git add umbra_website/plugins/plugin_directory/src/lib.rs umbra_website/plugins/plugin_directory/templates/plugin_directory/plugin.html
git commit -m "feat(notes): inline composer + reply forms + threaded live inserts"
```

---

### Task 4: Compact chat layout CSS

**Files:**
- Modify: `templates/plugin_directory/plugin.html` (the `extra_head` `<style>` block)

- [ ] **Step 1: Add the chat-surface styles**

In `plugin.html`'s `{% block extra_head %}` `<style>`, append:

```css
  /* ---- Notes chat surface ---- */
  .pd-composer { margin-top: 14px; border: 1px solid var(--hairline); border-radius: 14px; background: var(--surface); padding: 10px; }
  .pd-composer textarea { min-height: 0; resize: vertical; }
  .pd-composer-row { display: flex; gap: 8px; margin-top: 8px; align-items: center; }
  .pd-composer-row .pd-composer-name { flex: 1; }
  .pd-replies { margin: 6px 0 0 30px; display: flex; flex-direction: column; gap: 6px; border-left: 2px solid var(--hairline); padding-left: 12px; }
  .pd-reply { display: flex; gap: 8px; }
  .pd-reply-avatar { flex-shrink: 0; display: flex; align-items: center; justify-content: center; width: 24px; height: 24px; border-radius: 999px; background: var(--accent-line); color: var(--accent-2); font-size: 10.5px; font-weight: 700; }
  .pd-reply-head { font-size: 11.5px; color: var(--faint); }
  .pd-reply-name { font-weight: 700; color: var(--ink-2); }
  .pd-reply-toggle { margin: 4px 0 0 30px; background: transparent; border: 0; color: var(--accent-2); font-size: 12.5px; font-weight: 600; cursor: pointer; padding: 2px 0; }
  .pd-reply-form { margin: 6px 0 0 30px; }
  .pd-reply-form textarea { min-height: 0; resize: vertical; }
```

- [ ] **Step 2: Verify live + commit**

Reload `http://localhost:8100/plugins/umbra-admin#notes` and confirm the composer is compact, replies indent under their note with a rail, and the reply toggle reveals a tight form.

```bash
cd /home/dalmas/E/projects/umbra
git add umbra_website/plugins/plugin_directory/templates/plugin_directory/plugin.html
git commit -m "style(notes): compact chat layout for the notes thread"
```

---

## Final verification

- [ ] From `umbra_website/`: `cargo test -p plugin_directory --test render_pages` — all green (reply create/validate + thread grouping + existing note regression).
- [ ] Live: posting a note appends inline (no reload); "Reply" posts a nested reply that live-updates in another tab; the dialog is gone.
- [ ] A reply-to-a-reply / cross-plugin parent is rejected (covered by Task 1 test).

## Self-review notes

- **Spec coverage:** one-level replies + depth-1 guard (Task 1), inline composer replacing dialog (Task 3), lightweight replies — body+name only (Task 3 reply form), publish-then-moderate + honeypot + throttle inherited (Task 1/3 reuse `post_plugin_note` guards), SSE `{id,html,parent_id}` routing (Tasks 1+3), grouped one-query render (Task 2), compact layout (Task 4), slim `_reply.html` parity via `render_reply_row` (Task 2), tests (Tasks 1-2). All spec sections map to a task.
- **Type consistency:** `create_note(slug, body, kind, author_label, parent: Option<i64>)`, `NotePayload { id, html, parent_id: Option<i64> }`, `CommentThread { note, replies }`, `render_reply_row`, `PluginDetail.comments: Vec<CommentThread>`, `build(comments: Vec<CommentThread>)` — used consistently across tasks.
- **Placeholder scan:** none — every code step is complete; the Task 3 live reply-post reuses the documented CSRF/urlencoded flow from the earlier note-reload fix.
