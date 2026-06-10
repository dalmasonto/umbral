//! Demo-data seed — exercises every recent ORM/admin change in
//! one pass:
//!   ↪ Extra users + customers (1:1 demo via OneToOne<AuthUser> sugar)
//!   ↪ Addresses (FK to customer, AddressType choices)
//!   ↪ Coupons (DiscountType choices, DateTime range)
//!   ↪ Orders + items + payments + shipments (the full sales chain)
//!   ↪ Reviews (FK to Product + Customer, integer rating)
//!   ↪ Faqs + Testimonials (plain content variety)
//!
//! Idempotent — every step short-circuits on a non-empty table.
//! Must run BEFORE the blog seed because the blog comments
//! reference user ids 2 + 3 (alice + bob) which this seed creates.

use chrono::Utc;
use content::models::{Faq, Testimonial};
use ecommerce::models::{
    Address, AddressType, Coupon, Currency, Customer, DiscountType, Order, OrderItem, OrderStatus,
    Payment, PaymentMethod, PaymentStatus, Product, Review, Shipment,
};
use umbra::prelude::*;
use umbra_auth::AuthUser;

pub async fn demo_data() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = Utc::now();

    // ---- Extra users (for multiple customers + comment authors) ----
    for (name, email) in &[
        ("alice", "alice@example.com"),
        ("bob", "bob@example.com"),
        ("carol", "carol@example.com"),
    ] {
        if AuthUser::objects()
            .filter(umbra_auth::auth_user::USERNAME.eq(*name))
            .count()
            .await?
            == 0
        {
            umbra_auth::create_user(name, email, "demo-pw-12345").await?;
        }
    }

    // ---- Customers (1:1 with AuthUser via OneToOne<AuthUser> sugar) ----
    if Customer::objects().count().await? == 0 {
        let users = AuthUser::objects().fetch().await?;
        for (i, u) in users.iter().enumerate() {
            Customer::objects()
                .create(Customer {
                    id: 0,
                    user: umbra::orm::OneToOne::new(u.id),
                    phone: Some(format!("+15555550{:03}", 100 + i)),
                    date_of_birth: Some(
                        chrono::NaiveDate::from_ymd_opt(1990, 1, 1 + i as u32).unwrap(),
                    ),
                    accepts_marketing: i % 2 == 0,
                    loyalty_points: (i as i32) * 50,
                    created_at: now,
                    updated_at: now,
                })
                .await?;
        }
    }

    // ---- Addresses (billing + shipping per customer) ----
    if Address::objects().count().await? == 0 {
        let customers = Customer::objects().fetch().await?;
        for c in &customers {
            Address::objects()
                .bulk_create(vec![
                    Address {
                        id: 0,
                        customer: ForeignKey::new(c.id),
                        kind: AddressType::Billing,
                        line1: "1 Demo Street".into(),
                        line2: None,
                        city: "Nairobi".into(),
                        region: Some("Nairobi County".into()),
                        postal_code: "00100".into(),
                        country: "KE".into(),
                        is_default: true,
                    },
                    Address {
                        id: 0,
                        customer: ForeignKey::new(c.id),
                        kind: AddressType::Shipping,
                        line1: "1 Demo Street".into(),
                        line2: Some("Apt 4B".into()),
                        city: "Nairobi".into(),
                        region: Some("Nairobi County".into()),
                        postal_code: "00100".into(),
                        country: "KE".into(),
                        is_default: true,
                    },
                ])
                .await?;
        }
    }

    // ---- Coupons (one of each DiscountType variant) ----
    if Coupon::objects().count().await? == 0 {
        let valid_to = now + chrono::Duration::days(30);
        Coupon::objects()
            .bulk_create(vec![
                Coupon {
                    id: 0,
                    code: "WELCOME10".into(),
                    discount_type: DiscountType::Percentage,
                    value: "10".into(),
                    valid_from: now,
                    valid_to,
                    usage_limit: Some(1000),
                    used_count: 3,
                    is_active: true,
                },
                Coupon {
                    id: 0,
                    code: "FLAT5".into(),
                    discount_type: DiscountType::FixedAmount,
                    value: "5.00".into(),
                    valid_from: now,
                    valid_to,
                    usage_limit: None,
                    used_count: 0,
                    is_active: true,
                },
                Coupon {
                    id: 0,
                    code: "SHIPFREE".into(),
                    discount_type: DiscountType::FreeShipping,
                    value: "0".into(),
                    valid_from: now,
                    valid_to,
                    usage_limit: Some(50),
                    used_count: 0,
                    is_active: true,
                },
            ])
            .await?;
    }

    // ---- Orders + items + payments + shipments — the full sales chain ----
    if Order::objects().count().await? == 0 {
        let customers = Customer::objects().fetch().await?;
        let products = Product::objects().fetch().await?;
        if customers.is_empty() || products.is_empty() {
            return Ok(());
        }

        let orders_data = [
            (
                0_usize,
                OrderStatus::Pending,
                PaymentStatus::Pending,
                PaymentMethod::Card,
                "PEN-001",
            ),
            (
                0,
                OrderStatus::Paid,
                PaymentStatus::Captured,
                PaymentMethod::Mpesa,
                "PAID-002",
            ),
            (
                1,
                OrderStatus::Shipped,
                PaymentStatus::Captured,
                PaymentMethod::Card,
                "SHIP-003",
            ),
            (
                1,
                OrderStatus::Delivered,
                PaymentStatus::Captured,
                PaymentMethod::Paypal,
                "DLV-004",
            ),
            (
                2,
                OrderStatus::Cancelled,
                PaymentStatus::Refunded,
                PaymentMethod::Card,
                "CANCEL-005",
            ),
        ];
        for (i, (cust_idx, status, pay_status, method, number)) in orders_data.iter().enumerate() {
            let cust = &customers[*cust_idx % customers.len()];
            let prod = &products[i % products.len()];
            let qty = (i as i32 % 3) + 1;
            let unit_price: f64 = prod.price.parse().unwrap_or(20.0);
            let line_total = unit_price * (qty as f64);
            let shipping = 5.0;
            let tax = line_total * 0.08;
            let grand = line_total + shipping + tax;
            let placed_at = now - chrono::Duration::days((5 - i as i64).max(0));

            let order = Order::objects()
                .create(Order {
                    id: 0,
                    number: (*number).into(),
                    public_id: uuid::Uuid::new_v4(),
                    customer: ForeignKey::new(cust.id),
                    status: *status,
                    payment_status: *pay_status,
                    currency: Currency::Usd,
                    subtotal: format!("{:.2}", line_total),
                    shipping_total: format!("{:.2}", shipping),
                    tax_total: format!("{:.2}", tax),
                    discount_total: "0.00".into(),
                    grand_total: format!("{:.2}", grand),
                    coupon: None,
                    shipping_address: None,
                    billing_address: None,
                    notes: Some(format!("Demo order #{i}")),
                    invoice: None,
                    placed_at,
                    updated_at: placed_at,
                })
                .await?;

            OrderItem::objects()
                .create(OrderItem {
                    id: 0,
                    order: ForeignKey::new(order.id),
                    product: ForeignKey::new(prod.id),
                    variant: None,
                    quantity: qty,
                    unit_price: format!("{:.2}", unit_price),
                    line_total: format!("{:.2}", line_total),
                })
                .await?;

            // Payment only when not Pending.
            if !matches!(pay_status, PaymentStatus::Pending) {
                Payment::objects()
                    .create(Payment {
                        id: 0,
                        order: ForeignKey::new(order.id),
                        method: *method,
                        status: *pay_status,
                        amount: format!("{:.2}", grand),
                        currency: Currency::Usd,
                        transaction_id: Some(format!("txn_{}", order.id)),
                        paid_at: Some(placed_at + chrono::Duration::minutes(2)),
                    })
                    .await?;
            }

            // Shipment for Shipped + Delivered.
            if matches!(status, OrderStatus::Shipped | OrderStatus::Delivered) {
                Shipment::objects()
                    .create(Shipment {
                        id: 0,
                        order: ForeignKey::new(order.id),
                        carrier: "DHL".into(),
                        tracking_number: Some(format!("DHL-{}", order.id)),
                        shipped_at: Some(placed_at + chrono::Duration::hours(4)),
                        delivered_at: if matches!(status, OrderStatus::Delivered) {
                            Some(placed_at + chrono::Duration::days(2))
                        } else {
                            None
                        },
                    })
                    .await?;
            }
        }
    }

    // ---- Reviews (cross-product, integer rating) ----
    if Review::objects().count().await? == 0 {
        let customers = Customer::objects().fetch().await?;
        let products = Product::objects().fetch().await?;
        for (i, c) in customers.iter().enumerate().take(3) {
            for (j, p) in products.iter().enumerate().take(2) {
                Review::objects()
                    .create(Review {
                        id: 0,
                        product: ForeignKey::new(p.id),
                        customer: ForeignKey::new(c.id),
                        rating: ((i + j) as i32 % 5) + 1,
                        title: Some(format!("Review by user {i} for {}", p.name)),
                        body: "Quality build, fast shipping. Would buy again.".into(),
                        is_verified_purchase: true,
                        is_approved: true,
                        created_at: now,
                    })
                    .await?;
            }
        }
    }

    // ---- Faqs + Testimonials — content variety ----
    if Faq::objects().count().await? == 0 {
        Faq::objects()
            .bulk_create(vec![
                Faq {
                    id: 0,
                    question: "How long does shipping take?".into(),
                    answer: "Standard shipping is 3-5 business days within East Africa.".into(),
                    category: Some("Shipping".into()),
                    position: 0,
                    is_published: true,
                },
                Faq {
                    id: 0,
                    question: "Can I return an item?".into(),
                    answer: "Yes — within 30 days of delivery for a full refund.".into(),
                    category: Some("Returns".into()),
                    position: 1,
                    is_published: true,
                },
                Faq {
                    id: 0,
                    question: "Do you accept M-Pesa?".into(),
                    answer: "Yes, M-Pesa, card, PayPal, bank transfer, and COD.".into(),
                    category: Some("Payments".into()),
                    position: 2,
                    is_published: true,
                },
            ])
            .await?;
    }

    if Testimonial::objects().count().await? == 0 {
        Testimonial::objects()
            .bulk_create(vec![
                Testimonial {
                    id: 0,
                    author_name: "Alice Mwangi".into(),
                    author_title: Some("Designer".into()),
                    avatar: None,
                    quote: "Best place to grab solid hardware. Acme widgets always work.".into(),
                    rating: Some(5),
                    is_featured: true,
                    position: 0,
                },
                Testimonial {
                    id: 0,
                    author_name: "Brian Otieno".into(),
                    author_title: Some("Engineer".into()),
                    avatar: None,
                    quote: "Mechanical keyboard is the real deal. Switches feel premium.".into(),
                    rating: Some(5),
                    is_featured: true,
                    position: 1,
                },
            ])
            .await?;
    }

    Ok(())
}
