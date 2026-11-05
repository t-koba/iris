use std::collections::BTreeMap;
use std::path::PathBuf;

const TILE_SIZE: f64 = 256.0;
const MAX_MERCATOR_LAT: f64 = 85.051_128_78;
pub const DEFAULT_TILE_CACHE_LIMIT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpsBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GpsBounds {
    pub fn from_points(a: LatLon, b: LatLon) -> Self {
        Self {
            min_lat: a.lat.min(b.lat),
            max_lat: a.lat.max(b.lat),
            min_lon: a.lon.min(b.lon),
            max_lon: a.lon.max(b.lon),
        }
    }

    pub fn contains(self, point: LatLon) -> bool {
        point.lat >= self.min_lat
            && point.lat <= self.max_lat
            && point.lon >= self.min_lon
            && point.lon <= self.max_lon
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileSpan {
    pub z: u8,
    pub min_x: u32,
    pub max_x: u32,
    pub min_y: u32,
    pub max_y: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointCluster {
    pub count: usize,
    pub center: LatLon,
    pub bounds: GpsBounds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapViewport {
    pub center: LatLon,
    pub zoom: u8,
}

impl MapViewport {
    pub fn new(center: LatLon, zoom: u8) -> Self {
        Self {
            center,
            zoom: zoom.clamp(1, 18),
        }
    }

    pub fn fit_bounds(bounds: GpsBounds, width: f32, height: f32, padding: f32) -> Self {
        let width = f64::from(width.max(1.0));
        let height = f64::from(height.max(1.0));
        let padding = f64::from(padding.max(0.0));
        let available_width = (width - padding * 2.0).max(1.0);
        let available_height = (height - padding * 2.0).max(1.0);
        let center = LatLon {
            lat: (bounds.min_lat + bounds.max_lat) / 2.0,
            lon: (bounds.min_lon + bounds.max_lon) / 2.0,
        };
        let mut selected_zoom = 1;
        for zoom in 1..=18 {
            let top_left = lat_lon_to_world(
                LatLon {
                    lat: bounds.max_lat,
                    lon: bounds.min_lon,
                },
                zoom,
            );
            let bottom_right = lat_lon_to_world(
                LatLon {
                    lat: bounds.min_lat,
                    lon: bounds.max_lon,
                },
                zoom,
            );
            let span_x = (bottom_right.x - top_left.x).abs().max(1.0);
            let span_y = (bottom_right.y - top_left.y).abs().max(1.0);
            if span_x <= available_width && span_y <= available_height {
                selected_zoom = zoom;
            } else {
                break;
            }
        }
        Self::new(center, selected_zoom)
    }

    pub fn world_to_screen(self, point: LatLon, width: f32, height: f32) -> (f32, f32) {
        let center = lat_lon_to_world(self.center, self.zoom);
        let world = lat_lon_to_world(point, self.zoom);
        (
            (world.x - center.x) as f32 + width / 2.0,
            (world.y - center.y) as f32 + height / 2.0,
        )
    }

    pub fn screen_to_lat_lon(self, x: f32, y: f32, width: f32, height: f32) -> LatLon {
        let center = lat_lon_to_world(self.center, self.zoom);
        world_to_lat_lon(
            WorldPoint {
                x: center.x + f64::from(x - width / 2.0),
                y: center.y + f64::from(y - height / 2.0),
            },
            self.zoom,
        )
    }

    pub fn bounds_from_screen_rect(
        self,
        start: (f32, f32),
        end: (f32, f32),
        width: f32,
        height: f32,
    ) -> GpsBounds {
        GpsBounds::from_points(
            self.screen_to_lat_lon(start.0, start.1, width, height),
            self.screen_to_lat_lon(end.0, end.1, width, height),
        )
    }

    pub fn pan_by_pixels(&mut self, dx: f32, dy: f32) {
        let center = lat_lon_to_world(self.center, self.zoom);
        self.center = world_to_lat_lon(
            WorldPoint {
                x: center.x - f64::from(dx),
                y: center.y - f64::from(dy),
            },
            self.zoom,
        );
    }

    pub fn zoom_by_steps(&mut self, steps: i8) {
        let next = (i16::from(self.zoom) + i16::from(steps)).clamp(1, 18);
        self.zoom = next as u8;
    }

    pub fn visible_tile_span(self, width: f32, height: f32) -> TileSpan {
        let scale = scale_for_zoom(self.zoom);
        let center = lat_lon_to_world(self.center, self.zoom);
        let max_tile = (1_u32 << self.zoom).saturating_sub(1);
        let left = ((center.x - f64::from(width.max(1.0)) / 2.0) / TILE_SIZE)
            .floor()
            .clamp(0.0, f64::from(max_tile)) as u32;
        let right = ((center.x + f64::from(width.max(1.0)) / 2.0) / TILE_SIZE)
            .floor()
            .clamp(0.0, f64::from(max_tile)) as u32;
        let top = ((center.y - f64::from(height.max(1.0)) / 2.0) / TILE_SIZE)
            .floor()
            .clamp(0.0, f64::from(max_tile)) as u32;
        let bottom = ((center.y + f64::from(height.max(1.0)) / 2.0) / TILE_SIZE)
            .floor()
            .clamp(0.0, f64::from(max_tile)) as u32;
        let min_x = left.min(right);
        let max_x = left.max(right).min((scale / TILE_SIZE) as u32 - 1);
        let min_y = top.min(bottom);
        let max_y = top.max(bottom).min((scale / TILE_SIZE) as u32 - 1);
        TileSpan {
            z: self.zoom,
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }
}

pub fn lat_lon_to_world(point: LatLon, zoom: u8) -> WorldPoint {
    let scale = scale_for_zoom(zoom);
    let lat = point.lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
    let lon = wrap_lon(point.lon);
    let sin_lat = lat.to_radians().sin();
    let x = (lon + 180.0) / 360.0 * scale;
    let y = (0.5 - ((1.0 + sin_lat) / (1.0 - sin_lat)).ln() / (4.0 * std::f64::consts::PI)) * scale;
    WorldPoint { x, y }
}

pub fn world_to_lat_lon(point: WorldPoint, zoom: u8) -> LatLon {
    let scale = scale_for_zoom(zoom);
    let lon = point.x / scale * 360.0 - 180.0;
    let n = std::f64::consts::PI - 2.0 * std::f64::consts::PI * point.y / scale;
    let lat = n.sinh().atan().to_degrees();
    LatLon { lat, lon }
}

pub fn tile_coord(point: LatLon, zoom: u8) -> TileCoord {
    let world = lat_lon_to_world(point, zoom);
    let tiles = 1_u32 << zoom.min(31);
    TileCoord {
        z: zoom,
        x: ((world.x / TILE_SIZE).floor() as u32).min(tiles.saturating_sub(1)),
        y: ((world.y / TILE_SIZE).floor() as u32).min(tiles.saturating_sub(1)),
    }
}

pub fn tile_bounds(tile: TileCoord) -> GpsBounds {
    let top_left = world_to_lat_lon(
        WorldPoint {
            x: f64::from(tile.x) * TILE_SIZE,
            y: f64::from(tile.y) * TILE_SIZE,
        },
        tile.z,
    );
    let bottom_right = world_to_lat_lon(
        WorldPoint {
            x: f64::from(tile.x + 1) * TILE_SIZE,
            y: f64::from(tile.y + 1) * TILE_SIZE,
        },
        tile.z,
    );
    GpsBounds {
        min_lat: bottom_right.lat,
        max_lat: top_left.lat,
        min_lon: top_left.lon,
        max_lon: bottom_right.lon,
    }
}

pub fn visible_tiles(viewport: MapViewport, width: f32, height: f32) -> Vec<TileCoord> {
    let span = viewport.visible_tile_span(width, height);
    let tile_count =
        u64::from(span.max_x - span.min_x + 1) * u64::from(span.max_y - span.min_y + 1);
    if tile_count > 256 {
        return Vec::new();
    }
    let mut tiles = Vec::with_capacity(tile_count as usize);
    for y in span.min_y..=span.max_y {
        for x in span.min_x..=span.max_x {
            tiles.push(TileCoord { z: span.z, x, y });
        }
    }
    tiles
}

pub fn osm_tile_url(tile: TileCoord) -> String {
    format!(
        "https://tile.openstreetmap.org/{}/{}/{}.png",
        tile.z, tile.x, tile.y
    )
}

pub fn tile_cache_relative_path(tile: TileCoord) -> PathBuf {
    PathBuf::from(tile.z.to_string())
        .join(tile.x.to_string())
        .join(format!("{}.png", tile.y))
}

pub fn fit_bounds(points: &[LatLon]) -> Option<GpsBounds> {
    let first = *points.first()?;
    let mut bounds = GpsBounds {
        min_lat: first.lat,
        max_lat: first.lat,
        min_lon: first.lon,
        max_lon: first.lon,
    };
    for point in &points[1..] {
        bounds.min_lat = bounds.min_lat.min(point.lat);
        bounds.max_lat = bounds.max_lat.max(point.lat);
        bounds.min_lon = bounds.min_lon.min(point.lon);
        bounds.max_lon = bounds.max_lon.max(point.lon);
    }
    Some(bounds)
}

pub fn cluster_points(points: &[LatLon], zoom: u8, cell_px: f64) -> Vec<PointCluster> {
    if points.is_empty() || cell_px <= 0.0 {
        return Vec::new();
    }

    let mut buckets = BTreeMap::<(i64, i64), Vec<LatLon>>::new();
    for point in points {
        let world = lat_lon_to_world(*point, zoom);
        buckets
            .entry((
                (world.x / cell_px).floor() as i64,
                (world.y / cell_px).floor() as i64,
            ))
            .or_default()
            .push(*point);
    }

    buckets
        .into_values()
        .filter_map(|bucket| {
            let count = bucket.len();
            let bounds = fit_bounds(&bucket)?;
            let center = LatLon {
                lat: bucket.iter().map(|point| point.lat).sum::<f64>() / count as f64,
                lon: bucket.iter().map(|point| point.lon).sum::<f64>() / count as f64,
            };
            Some(PointCluster {
                count,
                center,
                bounds,
            })
        })
        .collect()
}

fn scale_for_zoom(zoom: u8) -> f64 {
    TILE_SIZE * 2_f64.powi(i32::from(zoom))
}

fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_coord_matches_web_mercator_origin() {
        assert_eq!(
            tile_coord(LatLon { lat: 0.0, lon: 0.0 }, 1),
            TileCoord { z: 1, x: 1, y: 1 }
        );
    }

    #[test]
    fn projection_round_trips_lat_lon() {
        let tokyo = LatLon {
            lat: 35.681_236,
            lon: 139.767_125,
        };
        let world = lat_lon_to_world(tokyo, 12);
        let round_trip = world_to_lat_lon(world, 12);
        assert!((round_trip.lat - tokyo.lat).abs() < 0.000_001);
        assert!((round_trip.lon - tokyo.lon).abs() < 0.000_001);
    }

    #[test]
    fn clusters_points_by_screen_cell() {
        let points = [
            LatLon {
                lat: 35.0,
                lon: 139.0,
            },
            LatLon {
                lat: 35.000_01,
                lon: 139.000_01,
            },
            LatLon {
                lat: 36.0,
                lon: 140.0,
            },
        ];
        let clusters = cluster_points(&points, 8, 64.0);
        assert_eq!(
            clusters.iter().map(|cluster| cluster.count).sum::<usize>(),
            3
        );
        assert!(clusters.iter().any(|cluster| cluster.count == 2));
    }

    #[test]
    fn viewport_fits_bounds_inside_available_area() {
        let bounds = GpsBounds {
            min_lat: 35.0,
            max_lat: 36.0,
            min_lon: 139.0,
            max_lon: 140.0,
        };
        let viewport = MapViewport::fit_bounds(bounds, 320.0, 180.0, 16.0);
        let top_left = viewport.world_to_screen(
            LatLon {
                lat: bounds.max_lat,
                lon: bounds.min_lon,
            },
            320.0,
            180.0,
        );
        let bottom_right = viewport.world_to_screen(
            LatLon {
                lat: bounds.min_lat,
                lon: bounds.max_lon,
            },
            320.0,
            180.0,
        );
        assert!(top_left.0 >= 16.0);
        assert!(top_left.1 >= 16.0);
        assert!(bottom_right.0 <= 304.0);
        assert!(bottom_right.1 <= 164.0);
    }

    #[test]
    fn viewport_screen_rect_converts_to_bounds() {
        let viewport = MapViewport::new(
            LatLon {
                lat: 35.5,
                lon: 139.5,
            },
            9,
        );
        let bounds = viewport.bounds_from_screen_rect((120.0, 60.0), (200.0, 140.0), 320.0, 180.0);
        assert!(bounds.min_lat < bounds.max_lat);
        assert!(bounds.min_lon < bounds.max_lon);
        assert!(bounds.contains(viewport.screen_to_lat_lon(160.0, 90.0, 320.0, 180.0)));
    }

    #[test]
    fn viewport_pan_changes_center() {
        let mut viewport = MapViewport::new(
            LatLon {
                lat: 35.5,
                lon: 139.5,
            },
            9,
        );
        let before = viewport.center;
        viewport.pan_by_pixels(20.0, 0.0);
        assert!(viewport.center.lon < before.lon);
    }

    #[test]
    fn visible_tiles_returns_slippy_tile_range() {
        let viewport = MapViewport::new(LatLon { lat: 0.0, lon: 0.0 }, 2);
        let tiles = visible_tiles(viewport, 512.0, 512.0);
        assert!(tiles.contains(&TileCoord { z: 2, x: 1, y: 1 }));
        assert!(tiles.contains(&TileCoord { z: 2, x: 2, y: 2 }));
        assert!(tiles.iter().all(|tile| tile.z == 2));
    }

    #[test]
    fn tile_url_and_cache_path_are_stable() {
        let tile = TileCoord {
            z: 8,
            x: 227,
            y: 100,
        };
        assert_eq!(
            osm_tile_url(tile),
            "https://tile.openstreetmap.org/8/227/100.png"
        );
        assert_eq!(
            tile_cache_relative_path(tile),
            PathBuf::from("8").join("227").join("100.png")
        );
    }

    #[test]
    fn tile_bounds_cover_tile_center() {
        let tile = TileCoord { z: 2, x: 2, y: 1 };
        let bounds = tile_bounds(tile);
        assert!(bounds.contains(world_to_lat_lon(
            WorldPoint {
                x: 2.5 * TILE_SIZE,
                y: 1.5 * TILE_SIZE,
            },
            2,
        )));
    }
}
