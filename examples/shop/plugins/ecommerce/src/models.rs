//! E-commerce plugin models.
//!
//! Catalog, sales, and review models. Content models (Category, Tag)
//! live in the content plugin and are referenced via FK.

use chrono::{DateTime, NaiveDate, Utc};
use content::{Category, Tag};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Choice enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ProductStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
    Kes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Pending,
    Paid,
    Fulfilled,
    Shipped,
    Delivered,
    Cancelled,
    Refunded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PaymentMethod {
    Card,
    Mpesa,
    Paypal,
    BankTransfer,
    Cod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    Authorized,
    Captured,
    Failed,
    Refunded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AddressType {
    Billing,
    Shipping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DiscountType {
    Percentage,
    FixedAmount,
    FreeShipping,
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Brand {
    pub id: i64,
    #[umbra(unique, string)]
    pub name: String,
    #[umbra(unique)]
    pub slug: String,
    pub logo: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Product {
    pub id: i64,
    #[umbra(unique)]
    pub sku: String,
    #[umbra(unique)]
    pub slug: String,
    #[umbra(string)]
    pub name: String,
    pub description: String,
    #[umbra(choices)]
    pub status: ProductStatus,
    pub category: ForeignKey<Category>,
    pub brand: Option<ForeignKey<Brand>>,
    pub price: String,
    pub compare_at_price: Option<String>,
    pub cost: String,
    #[umbra(choices)]
    pub currency: Currency,
    #[umbra(default = "0")]
    pub tax_rate: String,
    #[umbra(default = "0")]
    pub stock_quantity: i32,
    pub weight_kg: Option<f64>,
    pub dimensions: Option<serde_json::Value>,
    pub barcode: Option<String>,
    pub thumbnail: Option<String>,
    pub spec_sheet: Option<String>,
    #[umbra(default = "false")]
    pub is_featured: bool,
    #[umbra(default = "0")]
    pub rating_avg: f64,
    #[umbra(default = "0")]
    pub review_count: i32,
    pub metadata: serde_json::Value,
    pub keywords: Option<String>,
    pub external_id: Option<Uuid>,
    pub published_at: Option<DateTime<Utc>>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct ProductImage {
    pub id: i64,
    pub product: ForeignKey<Product>,
    pub image: String,
    pub alt_text: Option<String>,
    #[umbra(default = "0")]
    pub position: i32,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct ProductVariant {
    pub id: i64,
    pub product: ForeignKey<Product>,
    #[umbra(max_length = 64)]
    pub sku: String,
    pub attributes: serde_json::Value,
    pub price_override: Option<String>,
    #[umbra(default = "0")]
    pub stock_quantity: i32,
}

// ---------------------------------------------------------------------------
// Sales
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Customer {
    pub id: i64,
    /// Cross-crate 1:1 to AuthUser via the `OneToOne<T>` sugar
    /// (lives in umbra-auth; Customer lives here in ecommerce).
    /// Equivalent to `#[umbra(unique)] pub user: ForeignKey<AuthUser>`
    /// — emits a UNIQUE FK column AND exposes `auth_user.customer()
    /// .await?` as a back-link accessor on AuthUser via the
    /// cross-crate reverse-O2O trait.
    pub user: OneToOne<AuthUser>,
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    #[umbra(default = "false")]
    pub accepts_marketing: bool,
    pub loyalty_points: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Address {
    pub id: i64,
    pub customer: ForeignKey<Customer>,
    #[umbra(choices)]
    pub kind: AddressType,
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    pub region: Option<String>,
    pub postal_code: String,
    pub country: String,
    #[umbra(default = "false")]
    pub is_default: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Order {
    pub id: i64,
    #[umbra(unique)]
    pub number: String,
    pub public_id: Uuid,
    pub customer: ForeignKey<Customer>,
    #[umbra(choices)]
    pub status: OrderStatus,
    #[umbra(choices)]
    pub payment_status: PaymentStatus,
    #[umbra(choices)]
    pub currency: Currency,
    pub subtotal: String,
    pub shipping_total: String,
    pub tax_total: String,
    pub discount_total: String,
    pub grand_total: String,
    pub coupon: Option<ForeignKey<Coupon>>,
    pub shipping_address: Option<ForeignKey<Address>>,
    pub billing_address: Option<ForeignKey<Address>>,
    pub notes: Option<String>,
    pub invoice: Option<String>,
    pub placed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct OrderItem {
    pub id: i64,
    pub order: ForeignKey<Order>,
    pub product: ForeignKey<Product>,
    pub variant: Option<ForeignKey<ProductVariant>>,
    pub quantity: i32,
    pub unit_price: String,
    pub line_total: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Payment {
    pub id: i64,
    pub order: ForeignKey<Order>,
    #[umbra(choices)]
    pub method: PaymentMethod,
    #[umbra(choices)]
    pub status: PaymentStatus,
    pub amount: String,
    #[umbra(choices)]
    pub currency: Currency,
    pub transaction_id: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Shipment {
    pub id: i64,
    pub order: ForeignKey<Order>,
    pub carrier: String,
    pub tracking_number: Option<String>,
    pub shipped_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Coupon {
    pub id: i64,
    #[umbra(unique)]
    pub code: String,
    #[umbra(choices)]
    pub discount_type: DiscountType,
    pub value: String,
    pub valid_from: DateTime<Utc>,
    pub valid_to: DateTime<Utc>,
    pub usage_limit: Option<i32>,
    #[umbra(default = "0")]
    pub used_count: i32,
    #[umbra(default = "true")]
    pub is_active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Review {
    pub id: i64,
    pub product: ForeignKey<Product>,
    pub customer: ForeignKey<Customer>,
    pub rating: i32,
    pub title: Option<String>,
    pub body: String,
    #[umbra(default = "false")]
    pub is_verified_purchase: bool,
    #[umbra(default = "false")]
    pub is_approved: bool,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Many-to-many join tables (since ManyToMany<T> is not supported by derive)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct ProductTag {
    pub id: i64,
    pub product: ForeignKey<Product>,
    pub tag: ForeignKey<Tag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct CouponProduct {
    pub id: i64,
    pub coupon: ForeignKey<Coupon>,
    pub product: ForeignKey<Product>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct CouponCategory {
    pub id: i64,
    pub coupon: ForeignKey<Coupon>,
    pub category: ForeignKey<Category>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct ReviewImage {
    pub id: i64,
    pub review: ForeignKey<Review>,
    pub image: String,
}
