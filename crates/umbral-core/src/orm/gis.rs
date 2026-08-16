//! PostGIS spatial value type (`postgis` feature).
//!
//! [`Geometry`] is umbral's Rust binding for a PostGIS `geometry` / `geography`
//! column — a thin newtype over `geo_types::Geometry<f64>`, exactly the way
//! [`crate::orm::TsVector`] binds `tsvector`. It carries:
//!
//! - **sqlx `Type`/`Encode`/`Decode` for Postgres**, reading and writing EWKB
//!   (the binary wire form PostGIS speaks), delegated to `geozero`.
//! - **serde as GeoJSON**, so REST payloads and Leaflet/Mapbox clients consume
//!   it directly.
//!
//! The whole module is behind `#[cfg(feature = "postgis")]`: an app that never
//! declares a spatial column compiles none of the geo stack. The lightweight
//! [`crate::orm::SqlType::Geometry`] variant and its DDL/gating live in core
//! unconditionally; only this codec is gated.
//!
//! ## Wire strategy
//!
//! Writes flow through the ORM's JSON coerce path, so a geometry value reaches
//! the binder as a `serde_json::Value` (a GeoJSON object, or an EWKT / EWKB-hex
//! string). [`coerce_to_ewkt`] turns any of those into an EWKT string
//! (`SRID=4326;POINT(…)`) stamped with the column's declared SRID, which binds
//! straight into a `geometry` column via PostGIS's text→geometry cast — no
//! function-wrapping in the query builder. Reads decode EWKB into [`Geometry`]
//! and re-serialize as GeoJSON.

use geo_types::Geometry as GeoGeometry;
use geozero::{ToJson, ToWkt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A spatial value: a `geo_types::Geometry<f64>` that round-trips through a
/// PostGIS `geometry`/`geography` column (EWKB on the wire) and serializes as
/// GeoJSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry(pub GeoGeometry<f64>);

impl Geometry {
    /// The inner `geo_types` geometry.
    pub fn into_inner(self) -> GeoGeometry<f64> {
        self.0
    }

    /// EWKT with the given SRID, e.g. `SRID=4326;POINT(36.8 -1.3)`. This is the
    /// text form that binds directly into a PostGIS column.
    pub fn to_ewkt(&self, srid: i32) -> Result<String, String> {
        let wkt = self.0.to_wkt().map_err(|e| e.to_string())?;
        Ok(format!("SRID={srid};{wkt}"))
    }

    /// A GeoJSON `serde_json::Value` for this geometry.
    pub fn to_geojson_value(&self) -> Result<serde_json::Value, String> {
        let json = self.0.to_json().map_err(|e| e.to_string())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    }

    /// Parse a GeoJSON geometry `serde_json::Value` into a [`Geometry`].
    pub fn from_geojson_value(value: &serde_json::Value) -> Result<Geometry, String> {
        let s = value.to_string();
        Self::from_geojson_str(&s)
    }

    /// Parse a GeoJSON geometry string into a [`Geometry`].
    pub fn from_geojson_str(s: &str) -> Result<Geometry, String> {
        use geozero::ToGeo;
        use geozero::geojson::GeoJson;
        let geom = GeoJson(s).to_geo().map_err(|e| e.to_string())?;
        Ok(Geometry(geom))
    }
}

impl From<GeoGeometry<f64>> for Geometry {
    fn from(g: GeoGeometry<f64>) -> Self {
        Geometry(g)
    }
}

// --- serde: GeoJSON in both directions ------------------------------------

impl Serialize for Geometry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = self.to_geojson_value().map_err(serde::ser::Error::custom)?;
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Geometry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Geometry::from_geojson_value(&value).map_err(serde::de::Error::custom)
    }
}

// --- sqlx Postgres: EWKB codec via geozero --------------------------------

impl sqlx::Type<sqlx::Postgres> for Geometry {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        // PostGIS registers `geometry` (and `geography`) as extension types;
        // matching by name lets sqlx bind/decode them without a built-in oid.
        sqlx::postgres::PgTypeInfo::with_name("geometry")
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        use sqlx::TypeInfo;
        let name = ty.name().to_ascii_lowercase();
        name == "geometry" || name == "geography"
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Geometry {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let decoded: geozero::wkb::Decode<GeoGeometry<f64>> = sqlx::Decode::decode(value)?;
        decoded
            .geometry
            .map(Geometry)
            .ok_or_else(|| "decoded a NULL/empty PostGIS geometry".into())
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for Geometry {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        geozero::wkb::Encode(self.0.clone()).encode_by_ref(buf)
    }
}

// --- core coerce / decode helpers the SqlType arms call --------------------

/// Turn a geometry write value — a GeoJSON object, an EWKT string, or an
/// EWKB-hex string — into an EWKT string stamped with `srid`, ready to bind as
/// text into a PostGIS column. `srid == 0` leaves the value's own SRID intact
/// (no `SRID=` prefix is forced).
pub fn coerce_to_ewkt(value: &serde_json::Value, srid: i32) -> Result<String, String> {
    match value {
        // Already text: an EWKT (`SRID=…;POINT(…)`) or bare WKT, or EWKB hex.
        // Pass strings straight through — PostGIS's text→geometry cast parses
        // all of these. A bare WKT with no SRID relies on the column typmod.
        serde_json::Value::String(s) => Ok(s.clone()),
        // A GeoJSON geometry object: parse and re-emit as EWKT with the SRID.
        serde_json::Value::Object(_) => {
            let geom = Geometry::from_geojson_value(value)?;
            geom.to_ewkt(srid)
        }
        other => Err(format!(
            "expected a GeoJSON geometry object or a WKT/EWKT string, got {other}"
        )),
    }
}

/// Decode a Postgres geometry cell into a GeoJSON `serde_json::Value` (the
/// dynamic REST/admin read path). Nullable columns hand `Option<Geometry>`.
pub fn geometry_to_geojson(geom: &Geometry) -> Result<serde_json::Value, String> {
    geom.to_geojson_value()
}
