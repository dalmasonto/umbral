//! Catalog seed — categories, brands, products. Each step is
//! idempotent: a non-empty table short-circuits to a no-op so
//! repeated runs (or partial seeds from a previous boot) don't
//! double-insert.

use content::models::Category;
use ecommerce::models::{Brand, Currency, Product, ProductStatus};
use umbral::prelude::*;

pub async fn products() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if Category::objects().count().await? == 0 {
        Category::objects()
            .bulk_create(vec![
                Category {
                    id: 0,
                    slug: "gadgets".into(),
                    name: "Gadgets".into(),
                    description: Some("Tech and gadgets".into()),
                    image: None,
                    parent: None,
                    position: 0,
                    is_active: true,
                    test_field: None,
                },
                Category {
                    id: 0,
                    slug: "home".into(),
                    name: "Home & Living".into(),
                    description: Some("Home goods".into()),
                    image: None,
                    parent: None,
                    position: 1,
                    is_active: true,
                    test_field: None,
                },
            ])
            .await?;
    }

    if Brand::objects().count().await? == 0 {
        Brand::objects()
            .bulk_create(vec![
                Brand {
                    id: 0,
                    name: "Acme Corp".into(),
                    slug: "acme-corp".into(),
                    logo: None,
                    website: None,
                    description: Some("Quality gadgets since 1920".into()),
                },
                Brand {
                    id: 0,
                    name: "UmbralGear".into(),
                    slug: "umbralgear".into(),
                    logo: None,
                    website: None,
                    description: Some("Tools for developers".into()),
                },
                Brand {
                    id: 0,
                    name: "Rustic".into(),
                    slug: "rustic".into(),
                    logo: None,
                    website: None,
                    description: Some("Handcrafted home goods".into()),
                },
            ])
            .await?;
    }

    if Product::objects().count().await? == 0 {
        let brands = Brand::objects().fetch().await?;
        let brand_fk = brands.first().map(|b| ForeignKey::new(b.id));
        let now = chrono::Utc::now();

        Product::objects()
            .bulk_create(vec![
                Product {
                    id: 0,
                    sku: "ACM-001".into(),
                    slug: "acme-widget".into(),
                    name: "Acme Widget".into(),
                    description: "The original, the classic. A must-have for any gadget enthusiast.".into(),
                    status: ProductStatus::Active,
                    category: ForeignKey::new(1),
                    brand: brand_fk.clone(),
                    price: "29.99".into(),
                    compare_at_price: None,
                    cost: "15.00".into(),
                    currency: Currency::Usd,
                    tax_rate: "0.00".into(),
                    stock_quantity: 100,
                    weight_kg: Some(0.5),
                    dimensions: None,
                    barcode: None,
                    thumbnail: None,
                    spec_sheet: None,
                    is_featured: true,
                    rating_avg: 0.0,
                    review_count: 0,
                    metadata: serde_json::json!({}),
                    keywords: None,
                    external_id: None,
                    published_at: Some(now),
                    created_at: now,
                    updated_at: now,
                },
                Product {
                    id: 0,
                    sku: "UGR-101".into(),
                    slug: "developer-keyboard".into(),
                    name: "Mechanical Dev Keyboard".into(),
                    description: "Clicky switches, RGB lighting, and a Rust logo on the spacebar.".into(),
                    status: ProductStatus::Active,
                    category: ForeignKey::new(1),
                    brand: brand_fk.clone(),
                    price: "149.00".into(),
                    compare_at_price: Some("199.00".into()),
                    cost: "80.00".into(),
                    currency: Currency::Usd,
                    tax_rate: "0.00".into(),
                    stock_quantity: 45,
                    weight_kg: Some(1.2),
                    dimensions: None,
                    barcode: None,
                    thumbnail: None,
                    spec_sheet: None,
                    is_featured: true,
                    rating_avg: 0.0,
                    review_count: 0,
                    metadata: serde_json::json!({}),
                    keywords: None,
                    external_id: None,
                    published_at: Some(now),
                    created_at: now,
                    updated_at: now,
                },
                Product {
                    id: 0,
                    sku: "RUS-777".into(),
                    slug: "rustic-coffee-mug".into(),
                    name: "Rustic Coffee Mug".into(),
                    description: "Hand-thrown ceramic mug with a warm glaze. Holds 12oz of your favourite brew.".into(),
                    status: ProductStatus::Active,
                    category: ForeignKey::new(1),
                    brand: brand_fk.clone(),
                    price: "24.00".into(),
                    compare_at_price: None,
                    cost: "8.00".into(),
                    currency: Currency::Usd,
                    tax_rate: "0.00".into(),
                    stock_quantity: 200,
                    weight_kg: Some(0.4),
                    dimensions: None,
                    barcode: None,
                    thumbnail: None,
                    spec_sheet: None,
                    is_featured: false,
                    rating_avg: 0.0,
                    review_count: 0,
                    metadata: serde_json::json!({}),
                    keywords: None,
                    external_id: None,
                    published_at: Some(now),
                    created_at: now,
                    updated_at: now,
                },
            ])
            .await?;
    }

    Ok(())
}
