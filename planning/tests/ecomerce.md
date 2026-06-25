# Umbral — Reference Example App & Acceptance Ladder

**Status:** the repo is greenfield (only `arch.md` exists). Nothing runs yet. This document
defines the single example app that **stress-tests every feature A–Z** and the milestone ladder
for what becomes runnable at each stage. Build toward this app; let it be the acceptance test,
the tutorial, and the demo.

The example is a full **e-commerce store**, deliberately complex: one app, ~22 models spanning
catalog, sales, and content, touching every field type, every relationship kind, and every admin
capability. If umbral can render and operate this admin cleanly, it can handle real products.

---

## The app: `shop` (one app/plugin, all models)

Per the requirement, everything lives under a **single app** (`ShopPlugin`), grouped logically:

- **Catalog** — Category, Brand, Tag, Product, ProductImage, ProductVariant
- **Sales** — Customer, Address, Order, OrderItem, Payment, Shipment, Coupon, Review
- **Content** — Post, Comment, FAQ, ContactMessage, Subscriber, Page
- **Config** — StoreSetting (singleton)

All exercise the ORM via `#[derive(Model)]` and register with the admin via the plugin's admin
hook (so the sidebar shows one **Shop** group with all models).

---

## Models

Field options used below extend the ORM surface: `#[field(unique)]`, `index`, `default = …`,
`max_length = …`, `min`/`max` (validators), `auto_now_add` / `auto_now` (timestamps),
`backend = "postgres"`, and `#[model(unique_together = [...])]`. Choices are enums via
`#[derive(Choice)]`.

### Choice enums (→ render as pills/selects)
```rust
#[derive(Choice)] enum ProductStatus { Draft, Active, Archived }
#[derive(Choice)] enum Currency { Usd, Eur, Gbp, Kes }
#[derive(Choice)] enum OrderStatus { Pending, Paid, Fulfilled, Shipped, Delivered, Cancelled, Refunded }
#[derive(Choice)] enum PaymentMethod { Card, Mpesa, Paypal, BankTransfer, Cod }
#[derive(Choice)] enum PaymentStatus { Pending, Authorized, Captured, Failed, Refunded }
#[derive(Choice)] enum AddressType { Billing, Shipping }
#[derive(Choice)] enum DiscountType { Percentage, FixedAmount, FreeShipping }
#[derive(Choice)] enum ContactStatus { New, Read, Replied, Closed }
#[derive(Choice)] enum PostStatus { Draft, Published, Scheduled }
```

### Catalog

```rust
#[derive(Model)]
pub struct Category {                                   // self-referential tree
    #[field(unique, index)] pub slug: Slug,
    pub name: String,
    pub description: Option<String>,
    pub image: Option<ImageField>,
    pub parent: Option<ForeignKey<Category>>,           // SELF-FK (subcategories)
    #[field(default = 0)] pub position: i32,            // ordering
    #[field(default = true)] pub is_active: bool,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Brand {
    #[field(unique)] pub name: String,
    #[field(unique, index)] pub slug: Slug,
    pub logo: Option<ImageField>,
    pub website: Option<Url>,                           // URL field
    pub description: Option<String>,
}

#[derive(Model)]
pub struct Tag {                                        // shared by Product AND Post (M2M)
    #[field(unique)] pub name: String,
    #[field(unique, index)] pub slug: Slug,
}

#[derive(Model)]
#[model(indexes = ["status", "category"], ordering = ["-created_at"])]
pub struct Product {                                    // the heavy one — many column types
    #[field(unique, index, max_length = 64)] pub sku: String,
    #[field(unique, index)] pub slug: Slug,
    pub name: String,
    pub description: String,                            // long/rich text
    pub status: ProductStatus,                          // enum → pill
    pub category: ForeignKey<Category>,                 // FK (async picker)
    pub brand: Option<ForeignKey<Brand>>,               // nullable FK
    pub tags: ManyToMany<Tag>,                          // M2M (chips)
    #[field(min = 0)] pub price: Decimal,               // money + validator
    pub compare_at_price: Option<Decimal>,              // nullable money
    #[field(min = 0)] pub cost: Decimal,
    pub currency: Currency,
    #[field(default = 0)] pub tax_rate: Decimal,
    #[field(default = 0, min = 0)] pub stock_quantity: i32,
    pub weight_kg: Option<f64>,                         // float
    pub dimensions: Option<Json<Dimensions>>,           // JSON {l,w,h}
    pub barcode: Option<String>,
    pub thumbnail: Option<ImageField>,                  // image
    pub spec_sheet: Option<FileField>,                  // file (PDF preview!)
    #[field(default = false)] pub is_featured: bool,
    #[field(default = 0.0)] pub rating_avg: f64,        // denormalized (recomputed by task)
    #[field(default = 0)] pub review_count: i32,
    pub metadata: Json<serde_json::Value>,              // freeform JSON
    #[field(backend = "postgres")] pub keywords: ArrayField<String>,  // PG-ONLY → boot check
    pub external_id: Option<Uuid>,                      // UUID
    pub published_at: Option<DateTime>,                 // nullable datetime
    #[field(auto_now_add)] pub created_at: DateTime,
    #[field(auto_now)]     pub updated_at: DateTime,
}

#[derive(Model)]
#[model(ordering = ["position"])]
pub struct ProductImage {                               // inline under Product; ordered
    pub product: ForeignKey<Product>,
    pub image: ImageField,
    pub alt_text: Option<String>,
    #[field(default = 0)] pub position: i32,
}

#[derive(Model)]
#[model(unique_together = ["product", "sku"])]          // composite unique
pub struct ProductVariant {                             // inline under Product
    pub product: ForeignKey<Product>,
    #[field(max_length = 64)] pub sku: String,
    pub attributes: Json<serde_json::Value>,            // {size:"L", color:"Red"}
    pub price_override: Option<Decimal>,
    #[field(default = 0)] pub stock_quantity: i32,
}
```

### Sales

```rust
#[derive(Model)]
pub struct Customer {
    pub user: OneToOne<User>,                           // O2O with auth User
    pub phone: Option<String>,
    pub date_of_birth: Option<Date>,                    // Date (not datetime)
    #[field(default = false)] pub accepts_marketing: bool,
    pub loyalty_points: i32,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Address {                                    // inline under Customer
    pub customer: ForeignKey<Customer>,
    pub kind: AddressType,
    pub line1: String, pub line2: Option<String>,
    pub city: String, pub region: Option<String>,
    pub postal_code: String,
    pub country: String,                                // ISO code (choices in real life)
    #[field(default = false)] pub is_default: bool,
}

#[derive(Model)]
#[model(indexes = ["status", "customer"], ordering = ["-placed_at"])]
pub struct Order {
    #[field(unique, index)] pub number: String,         // e.g. ORD-2026-000123
    pub public_id: Uuid,                                // UUID for URLs
    pub customer: ForeignKey<Customer>,                 // FK (async picker, large table)
    pub status: OrderStatus,
    pub payment_status: PaymentStatus,
    pub currency: Currency,
    #[field(min = 0)] pub subtotal: Decimal,
    #[field(min = 0)] pub shipping_total: Decimal,
    #[field(min = 0)] pub tax_total: Decimal,
    #[field(min = 0)] pub discount_total: Decimal,
    #[field(min = 0)] pub grand_total: Decimal,
    pub coupon: Option<ForeignKey<Coupon>>,
    pub shipping_address: Option<ForeignKey<Address>>,
    pub billing_address: Option<ForeignKey<Address>>,
    pub notes: Option<String>,
    pub invoice: Option<FileField>,                     // generated PDF (file preview)
    pub placed_at: DateTime,
    #[field(auto_now)] pub updated_at: DateTime,
}

#[derive(Model)]
pub struct OrderItem {                                  // inline under Order; rich join row
    pub order: ForeignKey<Order>,
    pub product: ForeignKey<Product>,
    pub variant: Option<ForeignKey<ProductVariant>>,
    #[field(min = 1)] pub quantity: i32,
    #[field(min = 0)] pub unit_price: Decimal,
    #[field(min = 0)] pub line_total: Decimal,
}

#[derive(Model)]
pub struct Payment {
    pub order: ForeignKey<Order>,
    pub method: PaymentMethod,
    pub status: PaymentStatus,
    #[field(min = 0)] pub amount: Decimal,
    pub currency: Currency,
    pub transaction_id: Option<String>,
    pub paid_at: Option<DateTime>,
}

#[derive(Model)]
pub struct Shipment {
    pub order: ForeignKey<Order>,
    pub carrier: String,
    pub tracking_number: Option<String>,
    pub shipped_at: Option<DateTime>,
    pub delivered_at: Option<DateTime>,                 // nullable → drives status
}

#[derive(Model)]
#[model(indexes = ["code"])]
pub struct Coupon {
    #[field(unique, index)] pub code: String,
    pub discount_type: DiscountType,
    #[field(min = 0)] pub value: Decimal,
    pub valid_from: DateTime, pub valid_to: DateTime,   // date range
    pub usage_limit: Option<i32>,
    #[field(default = 0)] pub used_count: i32,
    #[field(default = true)] pub is_active: bool,
    pub applies_to_products: ManyToMany<Product>,       // M2M
    pub applies_to_categories: ManyToMany<Category>,    // M2M
}

#[derive(Model)]
#[model(unique_together = ["product", "customer"])]     // one review per product per customer
pub struct Review {
    pub product: ForeignKey<Product>,
    pub customer: ForeignKey<Customer>,
    #[field(min = 1, max = 5)] pub rating: i32,         // validated range
    pub title: Option<String>,
    pub body: String,
    #[field(default = false)] pub is_verified_purchase: bool,
    #[field(default = false)] pub is_approved: bool,    // moderation (bulk action)
    pub images: ManyToMany<ProductImage>,               // M2M (optional photos)
    #[field(auto_now_add)] pub created_at: DateTime,
}
```

### Content

```rust
#[derive(Model)]
#[model(ordering = ["-published_at"])]
pub struct Post {
    #[field(unique, index)] pub slug: Slug,
    pub title: String,
    pub excerpt: Option<String>,
    pub body: String,                                   // rich text
    pub status: PostStatus,
    pub author: ForeignKey<User>,                       // FK to auth
    pub category: Option<ForeignKey<Category>>,         // REUSES Category model
    pub tags: ManyToMany<Tag>,                          // REUSES Tag model
    pub cover_image: Option<ImageField>,
    pub attachment: Option<FileField>,                  // any file (preview/download)
    #[field(default = 0)] pub reading_minutes: i32,
    #[field(default = 0)] pub view_count: i64,          // BigInt
    pub published_at: Option<DateTime>,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Comment {                                    // inline under Post
    pub post: ForeignKey<Post>,
    pub author: ForeignKey<User>,
    pub body: String,
    #[field(default = false)] pub is_approved: bool,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
#[model(ordering = ["position"])]
pub struct Faq {
    pub question: String,
    pub answer: String,                                 // rich text
    pub category: Option<String>,                       // simple grouping
    #[field(default = 0)] pub position: i32,
    #[field(default = true)] pub is_published: bool,
}

#[derive(Model)]
#[model(ordering = ["-created_at"])]
pub struct ContactMessage {
    pub name: String,
    pub email: Email,                                   // Email field
    pub phone: Option<String>,
    pub subject: String,
    pub message: String,
    pub status: ContactStatus,                          // workflow enum
    pub ip_address: Option<String>,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Subscriber {
    #[field(unique, index)] pub email: Email,
    #[field(default = false)] pub is_confirmed: bool,
    pub confirmed_at: Option<DateTime>,
    pub source: Option<String>,
    #[field(auto_now_add)] pub created_at: DateTime,
}

#[derive(Model)]
pub struct Page {                                       // CMS flat page
    #[field(unique, index)] pub slug: Slug,
    pub title: String,
    pub content: String,                                // rich text
    #[field(default = false)] pub is_published: bool,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
}
```

### Config

```rust
#[derive(Model)]
#[model(singleton)]                                     // exactly one row
pub struct StoreSetting {
    pub store_name: String,
    pub default_currency: Currency,
    pub logo: Option<ImageField>,
    pub support_email: Email,
    pub config: Json<serde_json::Value>,
}
```

---

## Field-type & feature coverage

| Capability | Exercised by |
|---|---|
| Char / text / long text | `Product.name`, `Product.description`, `Page.content` |
| Slug (auto) | `slug` on Product/Category/Post/Page |
| Integer / BigInteger | `stock_quantity` (i32) · `Post.view_count` (i64) |
| Decimal (money) | all price/total/tax fields |
| Float | `Product.weight_kg`, `Product.rating_avg` |
| Boolean | `is_active`, `is_featured`, `is_approved`, … |
| DateTime / Date / Time | `created_at` · `Customer.date_of_birth` (Date) |
| Email / URL / UUID | `ContactMessage.email` · `Brand.website` · `Order.public_id` |
| JSON | `Product.metadata`, `dimensions`, `Variant.attributes` |
| **ArrayField (Postgres-only)** | `Product.keywords` → boot system-check test |
| Choices (enums → pills) | every `status`/`method`/`type`/`currency` field |
| File field (+ preview) | `Product.spec_sheet`, `Order.invoice`, `Post.attachment` |
| Image field (+ thumbnail) | `thumbnail`, `logo`, `cover_image`, `ProductImage.image` |
| Nullable → `Option<T>` | `compare_at_price`, `delivered_at`, `parent`, … |
| Unique / unique index | `sku`, `slug`, `Order.number`, `Coupon.code`, `email` |
| **unique_together** | `ProductVariant(product, sku)` · `Review(product, customer)` |
| Defaults | `is_active = true`, `position = 0`, … |
| Validators (min/max) | `price ≥ 0` · `Review.rating ∈ 1..=5` · `quantity ≥ 1` |
| Indexes / ordering (Meta) | `Product`, `Order`, `Post` Meta attributes |
| Auto timestamps | `auto_now_add` / `auto_now` across models |
| Denormalized/computed | `Product.rating_avg` + `review_count` (task-recomputed) |
| Singleton model | `StoreSetting` |

### Relationship coverage
| Kind | Exercised by |
|---|---|
| ForeignKey | Product→Category, Order→Customer, OrderItem→Product, … (dozens) |
| Nullable FK | Product→Brand, Order→Coupon, Post→Category |
| **Self-referential FK** | `Category.parent` (category tree) |
| OneToOne | `Customer.user` ↔ auth `User` |
| ManyToMany | `Product.tags`, `Coupon.applies_to_*`, `Review.images` |
| Shared model across features | `Category` & `Tag` used by **both** Product and Post |
| Rich join (through-with-fields) | `OrderItem` between Order ↔ Product |

---

## Admin feature coverage

| Admin feature | Exercised by |
|---|---|
| Inlines | ProductImage + ProductVariant under **Product**; OrderItem under **Order**; Address under **Customer**; Comment under **Post** |
| Async FK picker (no full load) | Order→Customer, OrderItem→Product (large tables) |
| M2M chips | Product↔Tag, Coupon↔Product/Category |
| File previews | `Order.invoice` (PDF), `Product.spec_sheet` (PDF), `Post.attachment` (any), images everywhere; a `.zip` export → download card |
| DataTable: filters/facets | Products by status/category/price-range; Orders by status/date-range/payment_status |
| DataTable: search | Products by name/sku; Orders by number/customer; Contact by email |
| DataTable: status pills | every enum column |
| Row actions (extensible) | "View invoice" (download PDF), "Duplicate product", "Refund order" |
| Bulk actions (floating toolbar) | "Mark orders fulfilled", "Approve reviews", "Publish posts", "Feature products", "Delete" |
| Right-side sheet preview/edit | every model; nested sheet when adding a Category from Product |
| Permissions | refund gated to managers; staff can edit but not delete orders |
| Custom theme (`admin.css`) | rebrand accent to the store's color |
| Dashboard widgets | see below |

### Dashboard widgets (the `shop` landing)
- **KPIs:** Revenue (30d) · Orders (today) · New customers (7d) · Avg order value.
- **Line chart:** Revenue over time (range selector).
- **Bar chart:** Sales by category.
- **Donut:** Orders by status.
- **Table widgets (reuse DataTable):** Top products · Low-stock products · Recent orders.
- **Feed:** Recent reviews awaiting approval.

### Background tasks (umbral-tasks)
- `send_order_confirmation(order_id)` — on checkout.
- `generate_invoice_pdf(order_id)` — populates `Order.invoice` (→ file preview).
- `recompute_product_rating(product_id)` — on new approved review (updates `rating_avg`/`review_count`).
- `low_stock_alert()` — scheduled/periodic; emails when `stock_quantity` < threshold.
- `send_newsletter(subscriber_ids)` — bulk.

---

## Acceptance ladder — what to run & assert at each milestone

| Milestone | First runnable thing | Test against the `shop` app |
|---|---|---|
| **M0** | App boots, one route | `cargo run -p umbral-cli -- runserver`; `curl /` → 200 |
| **M1** | QuerySet → SQL (hard-coded) | unit: build a `Product` filter, assert generated SQL |
| **M2–M3** | `#[derive(Model)]` | derive on `Product`; assert metadata + SQL match the hand impl; cover Decimal/JSON/enum/Array field mapping |
| **M4** | Boot system check | `Product.keywords` (ArrayField) on **SQLite** → clear error; on **Postgres** → boots |
| **M5** | **End-to-end migrations + ORM** | `makemigrations` → `migrate` the whole `shop` schema on Postgres; insert a Category tree, Products, an Order with OrderItems; query relations. Then add a field → migrate (ALTER); drop one → migrate (DROP) |
| **M6** | `inspectdb` | introspect an existing store DB → regenerate these models; assert round-trip |
| **M7** | Plugin trait | `ShopPlugin` registers all models + migrations; `migrate` walks it; sidebar shows one **Shop** group |
| **M8** | Hardened autodetect + plugins | self-FK (`Category.parent`) and unique_together migrate correctly; cross-plugin FK ordering (Customer→auth User); one rename detected |
| **M9** | Auth + sessions | `login_required`; manager-only refund permission enforced |
| **M10** | Tasks | checkout → `send_order_confirmation` enqueued; run worker; `generate_invoice_pdf` fills `Order.invoice`; periodic `low_stock_alert` |
| **M11** | REST (optional) | `/api/products` JSON with pagination + `?category=&status=` filter; confirm REST-free build still compiles |
| **M12** | **Admin + OpenAPI** | full admin: DataTable (filters/search/pills/pagination/bulk toolbar), right-sheet preview/edit with inlines, async pickers, **PDF/image previews**, extensible refund/invoice actions, dashboard widgets; `/api/docs` Swagger |
| **M13** | Polish | `startapp` generator; autoreload; CSV export; custom `admin.css` rebrand |

> **M5** is still the "it works" moment, but now it proves a 22-model relational schema migrates
> and queries — far more convincing than a blog. **M12** is the showcase: this admin is the demo
> that sells the framework.

---

## One-command smoke test (M5+)
```bash
cargo run -p umbral-cli -- makemigrations
cargo run -p umbral-cli -- migrate
cargo run -p umbral-cli -- loaddata shop_seed.json   # seed categories, products, an order
cargo run -p umbral-cli -- shell <<< 'assert Product::objects().count() > 0; \
  assert Order::objects().filter(status=OrderStatus::Paid).count() >= 0'
```
Grow into a real integration test (`axum-test` + a throwaway Postgres) as features land. A seed
fixture (`shop_seed.json`) doubles as demo data for the admin showcase.
