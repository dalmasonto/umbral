# umbral-content — Common Content & Site Models Plugin

| | |
|---|---|
| **Type** | Optional **first-party** plugin (like `umbral-rest`) — installed, not forced |
| **Purpose** | The universal content/site/CMS models almost every app needs |
| **Status** | Draft v0.1 · May 30, 2026 |
| **Companion** | `arch.md` (plugin contract) · `umbral-example-app.md` (the `shop` app reuses this) |

---

## 1. Why this exists

Every web app re-implements the same handful of models: a blog, static pages, an FAQ, a contact
form, a newsletter list, navigation menus, site settings. `umbral-content` ships them once,
correctly, so a new umbral project gets a working content layer by adding one plugin — the same
"batteries included" promise that makes the framework worth choosing over bare Axum.

It is a **normal plugin** (implements `Plugin`, owns its migrations, registers admin models). The
thin core does not depend on it; you opt in:

```rust
App::builder()
    .plugin(AuthPlugin::default())
    .plugin(AdminPlugin::default())
    .plugin(ContentPlugin::default())   // ← adds Post, Page, Faq, Menu, etc. + their migrations
    .plugin(ShopPlugin)                 // domain app, can reuse content's Category/Tag
    .build();
```

On `migrate`, its tables are created automatically; in the admin sidebar its models appear under
a **Content** group (scoped by `plugin.name()`), beside **Shop**, **Auth**, etc.

---

## 2. Relationship to the `shop` example

The `shop` example currently defines Post/Comment/Page/Faq/ContactMessage/Subscriber/Category/Tag
inline. Those move **here**, and `shop` depends on `umbral-content` and reuses its `Category` and
`Tag` for products — demonstrating **cross-plugin model reuse** and cross-plugin FK ordering on
`migrate`. `shop` keeps only e-commerce-specific models (Product, Order, …).

---

## 3. Choice enums
```rust
#[derive(Choice)] enum PostStatus    { Draft, Published, Scheduled }
#[derive(Choice)] enum ContactStatus { New, Read, Replied, Closed }
#[derive(Choice)] enum RedirectCode  { MovedPermanently /*301*/, Found /*302*/ }
#[derive(Choice)] enum PageTemplate  { Default, FullWidth, Landing }
```

---

## 4. Models

### 4.1 Taxonomy (shared across content types and by other plugins)
```rust
#[derive(Model)]
pub struct Category {                                 // self-referential tree, reusable
    #[field(unique, index)] pub slug: Slug,
    pub name: String,
    pub description: Option<String>,
    pub image: Option<ImageField>,
    pub parent: Option<ForeignKey<Category>>,         // SELF-FK
    #[field(default = 0)]    pub position: i32,
    #[field(default = true)] pub is_active: bool,
}

#[derive(Model)]
pub struct Tag {
    #[field(unique)] pub name: String,
    #[field(unique, index)] pub slug: Slug,
}
```

### 4.2 Blog
```rust
#[derive(Model)]
#[model(indexes = ["status"], ordering = ["-published_at"])]
pub struct Post {
    #[field(unique, index)] pub slug: Slug,
    pub title: String,
    pub excerpt: Option<String>,
    pub body: String,                                 // rich text
    pub status: PostStatus,
    pub author: ForeignKey<User>,                     // FK to auth
    pub category: Option<ForeignKey<Category>>,
    pub tags: ManyToMany<Tag>,
    pub cover_image: Option<ImageField>,
    pub attachment: Option<FileField>,                // file preview/download
    #[field(default = false)] pub is_featured: bool,
    #[field(default = 0)]  pub reading_minutes: i32,
    #[field(default = 0)]  pub view_count: i64,       // BigInt
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime>,
    #[field(auto_now_add)] pub created_at: DateTime,
    #[field(auto_now)]     pub updated_at: DateTime,
}

#[derive(Model)]
#[model(ordering = ["created_at"])]
pub struct Comment {                                  // threaded; inline under Post in admin
    pub post: ForeignKey<Post>,
    pub parent: Option<ForeignKey<Comment>>,          // SELF-FK (threads)
    pub author: Option<ForeignKey<User>>,             // null = anonymous
    pub author_name: Option<String>,                  // for anonymous
    pub author_email: Option<Email>,
    pub body: String,
    #[field(default = false)] pub is_approved: bool,  // moderation (bulk action)
    #[field(auto_now_add)] pub created_at: DateTime,
}
```

### 4.3 Pages / CMS
```rust
#[derive(Model)]
#[model(ordering = ["position"])]
pub struct Page {                                     // static pages: About, Terms, Privacy…
    #[field(unique, index)] pub slug: Slug,
    pub title: String,
    pub content: String,                              // rich text
    pub template: PageTemplate,
    pub parent: Option<ForeignKey<Page>>,             // SELF-FK (nested pages)
    #[field(default = 0)]     pub position: i32,
    #[field(default = false)] pub is_published: bool,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime>,
    #[field(auto_now)] pub updated_at: DateTime,
}

#[derive(Model)]
#[model(ordering = ["position"])]
pub struct Faq {
    pub question: String,
    pub answer: String,                               // rich text
    pub category: Option<String>,                     // simple grouping label
    #[field(default = 0)]    pub position: i32,
    #[field(default = true)] pub is_published: bool,
}
```

### 4.4 Navigation
```rust
#[derive(Model)]
pub struct Menu {
    #[field(unique)] pub name: String,
    #[field(unique, index)] pub slug: Slug,           // e.g. "main", "footer"
}

#[derive(Model)]
#[model(ordering = ["position"])]
pub struct MenuItem {                                 // inline under Menu in admin
    pub menu: ForeignKey<Menu>,
    pub parent: Option<ForeignKey<MenuItem>>,         // SELF-FK (submenus)
    pub label: String,
    pub url: Option<Url>,                             // external link…
    pub page: Option<ForeignKey<Page>>,              // …or link to a Page
    #[field(default = 0)] pub position: i32,
    #[field(default = "_self")] pub target: String,
    #[field(default = true)] pub is_active: bool,
}
```

### 4.5 Marketing
```rust
#[derive(Model)]
#[model(ordering = ["position"])]
pub struct Banner {                                   // hero / announcement bars
    pub title: String,
    pub content: Option<String>,
    pub image: Option<ImageField>,
    pub link_url: Option<Url>,
    pub starts_at: Option<DateTime>,                  // date range scheduling
    pub ends_at: Option<DateTime>,
    #[field(default = 0)]     pub position: i32,
    #[field(default = true)]  pub is_active: bool,
}

#[derive(Model)]
#[model(ordering = ["position"])]
pub struct Testimonial {
    pub author_name: String,
    pub author_title: Option<String>,
    pub avatar: Option<ImageField>,
    pub quote: String,
    #[field(min = 1, max = 5)] pub rating: Option<i32>,
    #[field(default = false)]  pub is_featured: bool,
    #[field(default = 0)]      pub position: i32,
}
```

### 4.6 Communication
```rust
#[derive(Model)]
#[model(ordering = ["-created_at"])]
pub struct ContactMessage {                           // contact-form submissions
    pub name: String,
    pub email: Email,
    pub phone: Option<String>,
    pub subject: String,
    pub message: String,
    pub status: ContactStatus,                        // workflow enum → pill
    pub ip_address: Option<String>,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Subscriber {                               // newsletter list
    #[field(unique, index)] pub email: Email,
    #[field(default = false)] pub is_confirmed: bool,
    pub confirmed_at: Option<DateTime>,
    pub source: Option<String>,
    #[field(auto_now_add)] pub created_at: DateTime,
}
```

### 4.7 Media library
```rust
#[derive(Model)]
#[model(ordering = ["-created_at"])]
pub struct MediaAsset {                               // central uploads (file previews)
    pub file: FileField,                              // any file → preview_kind resolved
    pub title: Option<String>,
    pub alt_text: Option<String>,
    pub folder: Option<String>,                       // simple grouping
    pub mime: String,
    pub size_bytes: i64,
    pub uploaded_by: Option<ForeignKey<User>>,
    #[field(auto_now_add)] pub created_at: DateTime,
}
```

### 4.8 SEO & config
```rust
#[derive(Model)]
pub struct Redirect {                                 // SEO redirects
    #[field(unique, index)] pub from_path: String,
    pub to_path: String,
    pub code: RedirectCode,                           // 301 / 302
    #[field(default = true)] pub is_active: bool,
    #[field(default = 0)]    pub hits: i64,
}

#[derive(Model)]
#[model(singleton)]                                   // exactly one row
pub struct SiteSetting {
    pub site_name: String,
    pub tagline: Option<String>,
    pub logo: Option<ImageField>,
    pub favicon: Option<ImageField>,
    pub contact_email: Email,
    pub social_links: Json<serde_json::Value>,        // {twitter, linkedin, …}
    pub default_seo: Json<serde_json::Value>,
    pub config: Json<serde_json::Value>,
}
```

---

## 5. Admin & plugin integration

- **Registration:** `ContentPlugin` registers each model with the admin (sidebar group
  **Content**) and exposes its migrations via the `Plugin` contract — no special-casing.
- **Inlines:** Comment under Post; MenuItem under Menu; (Page children shown as a tree).
- **Bulk actions:** approve comments, publish posts/pages, mark contact messages read,
  confirm subscribers, activate/deactivate banners.
- **File previews:** MediaAsset and Post.attachment exercise the §10 preview system (images,
  PDFs, etc.).
- **Self-FK trees:** Category, Page, Comment, MenuItem all exercise self-referential FK migration
  + admin tree rendering.
- **Singleton:** `SiteSetting` shows the singleton edit pattern (no list; straight to the sheet).

---

## 6. Feature coverage delta (what this plugin adds to the test surface)

| Capability | Exercised by |
|---|---|
| Self-referential FK (×4) | Category, Page, Comment, MenuItem |
| Optional author / anonymous pattern | Comment (`author` null + `author_name`/`author_email`) |
| "Link to internal page OR external URL" | MenuItem (`page` FK vs `url`) |
| Date-range scheduling | Banner (`starts_at`/`ends_at`) |
| Singleton model | SiteSetting |
| Central media library + file previews | MediaAsset |
| Cross-plugin reuse | `shop` reuses `Category`/`Tag` from here |

---

## 7. Crate placement

Add to the workspace plugin list (alongside the others in `arch.md` / `CLAUDE.md`):

```
plugins/
  umbral-content   # OPTIONAL first-party: Post, Page, Faq, Menu, Banner, Media, SiteSetting, …
```

Depends only on the `umbral` facade (+ `umbral-auth` for the `User` FK). Apps that want a content
layer add `ContentPlugin`; apps that don't, omit it.
