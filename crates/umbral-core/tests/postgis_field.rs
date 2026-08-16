// The whole suite needs the `postgis` feature (the `gis::Geometry` field type
// only compiles then). Build/run with: `--features postgis`.
#![cfg(feature = "postgis")]
#![allow(dead_code, private_interfaces)]

//! PostGIS `geometry` end-to-end (gaps5 #22).
//!
//! Coverage:
//! - **Derive classification.** A `gis::Geometry` field with
//!   `#[umbral(geometry = "point", srid = 4326)]` lands as
//!   `SqlType::Geometry(GeometrySpec { Point, 4326 })`.
//! - **DDL.** Postgres renders `geometry(Point,4326)`, emits
//!   `CREATE EXTENSION IF NOT EXISTS postgis`, and a GiST index for an
//!   `#[umbral(index)]` spatial column.
//! - **Live round-trip with REAL data.** Kenyan healthcare facilities (Points)
//!   and county boundaries (Polygons) are written through the ORM's coerce
//!   path (GeoJSON → EWKT), read back both dynamically (GeoJSON) and typed
//!   (`Geometry` decode), and queried with `ST_DWithin` / `ST_Intersects`.
//!
//! The live test self-skips unless `UMBRAL_TEST_POSTGRES_URL` is set AND the
//! amenities-radar data files are present.

use serde_json::json;
use umbral::migrate::ModelMeta;
use umbral::orm::{DynQuerySet, GeometryKind, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_postgis_facility")]
struct Facility {
    id: i64,
    name: String,
    county: String,
    #[umbral(geometry = "point", srid = 4326, index)]
    location: umbral_core::orm::gis::Geometry,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_postgis_county")]
struct County {
    id: i64,
    name: String,
    // Unconstrained subtype: real admin boundaries mix Polygon and
    // MultiPolygon, so the column accepts any geometry (still SRID-locked).
    #[umbral(geometry = "geometry", srid = 4326)]
    boundary: umbral_core::orm::gis::Geometry,
}

#[test]
fn derive_classifies_geometry_with_subtype_and_srid() {
    let f: std::collections::HashMap<&str, &umbral::orm::FieldSpec> = <Facility as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();
    let loc = f.get("location").expect("location field");
    match loc.ty {
        SqlType::Geometry(spec) => {
            assert_eq!(spec.kind, GeometryKind::Point);
            assert_eq!(spec.srid, 4326);
        }
        other => panic!("expected SqlType::Geometry(Point,4326), got {other:?}"),
    }
    assert!(
        loc.index,
        "#[umbral(index)] must mark the column for a GiST index"
    );

    let c: std::collections::HashMap<&str, &umbral::orm::FieldSpec> = <County as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();
    match c.get("boundary").unwrap().ty {
        SqlType::Geometry(spec) => assert_eq!(spec.kind, GeometryKind::Geometry),
        other => panic!("expected unconstrained Geometry, got {other:?}"),
    }
}

#[test]
fn postgres_ddl_renders_typmod_extension_and_gist() {
    use umbral::migrate::{Column, Operation, render_operation_for};

    let cols: Vec<Column> = <Facility as Model>::FIELDS
        .iter()
        .map(Column::from)
        .collect();
    let op = Operation::CreateTable {
        table: "umbral_postgis_facility".to_string(),
        columns: cols,
        indexes: Vec::new(),
        unique_together: Vec::new(),
    };
    let sql = render_operation_for(&op, "postgres").join("\n");
    let lower = sql.to_lowercase();
    assert!(
        lower.contains("geometry(point,4326)"),
        "must render the typmod geometry(Point,4326); got:\n{sql}"
    );
    assert!(
        lower.contains("create extension if not exists postgis"),
        "a spatial table must auto-create the postgis extension; got:\n{sql}"
    );
    assert!(
        lower.contains("using gist"),
        "an #[umbral(index)] spatial column must get a GiST index; got:\n{sql}"
    );
}

// ---------------------------------------------------------------------------
// Live Postgres end-to-end with the real Kenyan datasets.
// ---------------------------------------------------------------------------

const FACILITIES: &str = "/home/dalmas/E/projects/amenities-radar/data/healthcare_facilities.json";
const BOUNDARIES: &str = "/home/dalmas/E/projects/amenities-radar/ken_admin_boundaries.geojson.zip";

/// Load up to `limit` facility Points as (name, county, geojson-geometry).
fn load_facilities(limit: usize) -> Option<Vec<(String, String, serde_json::Value)>> {
    let bytes = std::fs::read(FACILITIES).ok()?;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let feats = doc.get("features")?.as_array()?;
    let mut out = Vec::new();
    for feat in feats.iter().take(limit) {
        let geom = feat.get("geometry")?.clone();
        let props = feat.get("properties")?;
        let name = props
            .get("Facility_N")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .trim()
            .to_string();
        let county = props
            .get("County")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        out.push((name, county, geom));
    }
    Some(out)
}

/// Load the 47 county Polygons as (name, geojson-geometry) from the zip.
fn load_counties() -> Option<Vec<(String, serde_json::Value)>> {
    let file = std::fs::File::open(BOUNDARIES).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut buf = String::new();
    {
        use std::io::Read;
        let mut entry = zip.by_name("ken_admin1.geojson").ok()?;
        entry.read_to_string(&mut buf).ok()?;
    }
    let doc: serde_json::Value = serde_json::from_str(&buf).ok()?;
    let feats = doc.get("features")?.as_array()?;
    let mut out = Vec::new();
    for feat in feats {
        let geom = feat.get("geometry")?.clone();
        let name = feat
            .get("properties")?
            .get("adm1_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        out.push((name, geom));
    }
    Some(out)
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL + PostGIS + the amenities-radar data files"]
async fn postgis_real_data_round_trip_and_spatial_queries() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let (Some(facilities), Some(counties)) = (load_facilities(800), load_counties()) else {
        eprintln!("skipping: amenities-radar data files not found");
        return;
    };
    assert_eq!(counties.len(), 47, "Kenya has 47 counties in admin1");

    let pool = umbral_core::db::connect_postgres(&url)
        .await
        .expect("pg pool");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Facility>()
        .model::<County>()
        .build()
        .expect("App::build (geometry is valid on Postgres)");

    // Fresh tables through the ORM migration DDL path — this exercises the
    // `geometry(Point,4326)` typmod, `CREATE EXTENSION postgis`, and the GiST
    // index end to end (a failure here means the generated DDL is wrong).
    for t in ["umbral_postgis_facility", "umbral_postgis_county"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create spatial tables");

    // A GiST index must exist on the facility location column.
    let gist: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes \
         WHERE tablename = 'umbral_postgis_facility' AND indexdef ILIKE '%gist%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(gist >= 1, "the GiST index must have been created");

    // WRITE the real data through the ORM coerce path (GeoJSON -> EWKT).
    let fac_meta = ModelMeta::for_::<Facility>();
    let mut first_returned: Option<serde_json::Map<String, serde_json::Value>> = None;
    for (name, county, geom) in &facilities {
        let mut body = serde_json::Map::new();
        body.insert("name".into(), json!(name));
        body.insert("county".into(), json!(county));
        body.insert("location".into(), geom.clone());
        let returned = DynQuerySet::for_meta(&fac_meta)
            .insert_json(&body)
            .await
            .unwrap_or_else(|e| panic!("insert facility {name}: {e:?}"));
        if first_returned.is_none() {
            first_returned = Some(returned);
        }
    }
    // The row the write returned carries the geometry re-serialised as GeoJSON
    // (the dynamic read path). This is what REST/admin clients receive.
    let loc = first_returned
        .as_ref()
        .and_then(|r| r.get("location"))
        .expect("returned row carries a GeoJSON location");
    assert_eq!(
        loc.get("type").and_then(|v| v.as_str()),
        Some("Point"),
        "dynamic write/read must serialise geometry as GeoJSON; got {loc}"
    );
    let county_meta = ModelMeta::for_::<County>();
    for (name, geom) in &counties {
        let mut body = serde_json::Map::new();
        body.insert("name".into(), json!(name));
        body.insert("boundary".into(), geom.clone());
        DynQuerySet::for_meta(&county_meta)
            .insert_json(&body)
            .await
            .unwrap_or_else(|e| panic!("insert county {name}: {e:?}"));
    }

    // Typed read-back: `Geometry` decodes straight from EWKB, and the row count
    // matches what we wrote.
    let rows = Facility::objects().fetch_pg(&pool).await.expect("fetch_pg");
    assert_eq!(rows.len(), facilities.len());
    // Every decoded location is a Point (the geo_types variant).
    assert!(
        matches!(rows[0].location.0, geo_types::Geometry::Point(_)),
        "decoded geometry should be a Point"
    );

    let total = facilities.len();
    // Spatial query 1: facilities within ~0.6 degrees of central Nairobi. A
    // real filter must return a PROPER subset — some rows, but not all (the
    // dataset spans the whole country), which proves it actually filters.
    let nairobi = "SRID=4326;POINT(36.8172 -1.2864)";
    let near_deg = Facility::objects()
        .filter(facility::LOCATION.dwithin(nairobi, 0.6))
        .count()
        .await
        .expect("dwithin count");
    eprintln!("[postgis] within 0.6deg of Nairobi: {near_deg} / {total} facilities");
    assert!(
        near_deg > 0 && (near_deg as usize) < total,
        "ST_DWithin must return a proper subset near Nairobi; got {near_deg} of {total}"
    );

    // Spatial query 1b: the METRES semantics the REST `__dwithin` filter uses —
    // `ST_DWithin(col::geography, point::geography, meters)`. Within 20 km of
    // Nairobi should be MORE than within 5 km (monotonic), and both a subset.
    let count_within_m = |meters: f64| {
        let pool = pool.clone();
        async move {
            let sql = "SELECT count(*) FROM umbral_postgis_facility \
                       WHERE ST_DWithin(location::geography, \
                             ST_SetSRID(ST_MakePoint($1, $2), 4326)::geography, $3)";
            sqlx::query_scalar::<_, i64>(sql)
                .bind(36.8172_f64)
                .bind(-1.2864_f64)
                .bind(meters)
                .fetch_one(&pool)
                .await
                .expect("dwithin meters")
        }
    };
    let within_5km = count_within_m(5_000.0).await;
    let within_20km = count_within_m(20_000.0).await;
    eprintln!("[postgis] within 5km: {within_5km}, within 20km: {within_20km}, total: {total}");
    assert!(
        within_5km > 0 && within_5km <= within_20km && (within_20km as usize) < total,
        "metres dwithin must be monotonic and a subset: 5km={within_5km} 20km={within_20km} total={total}"
    );

    // Spatial query 2: facilities whose Point intersects a specific county
    // Polygon. Pick the county with the most facilities in our sample so the
    // count is reliably > 0.
    use std::collections::HashMap;
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for (_, county, _) in &facilities {
        *freq.entry(county.as_str()).or_default() += 1;
    }
    let top_county = freq
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(c, _)| *c)
        .unwrap();
    // Find that county's polygon EWKT via PostGIS.
    let poly_ewkt: Option<String> = sqlx::query_scalar(
        "SELECT ST_AsEWKT(boundary) FROM umbral_postgis_county WHERE name ILIKE $1 LIMIT 1",
    )
    .bind(top_county)
    .fetch_optional(&pool)
    .await
    .unwrap();

    if let Some(poly) = poly_ewkt {
        let inside = Facility::objects()
            .filter(facility::LOCATION.intersects(&poly))
            .count()
            .await
            .expect("intersects count");
        assert!(
            inside > 0,
            "ST_Intersects must find facilities inside {top_county}; got {inside}"
        );
    }
}
