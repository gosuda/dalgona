//! SVG raster hardening: href blocking, size caps, deterministic PNG output.

use std::sync::Arc;

use super::DiagramOutcome;

const PNG_CAP: usize = 4 * 1_024 * 1_024;
const PIXEL_CAP: u32 = 32 * 1_024 * 1_024;
const AXIS_CAP: f32 = 16_384.0;

/// Renders SVG bytes to PNG pixels with hardening caps.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "dims are finiteness- and cap-checked; a saturating cast falls to the pixmap cap"
)]
pub fn render_svg(svg: &[u8], kind: super::DiagramKind) -> DiagramOutcome {
    if svg.len() > PNG_CAP {
        return fallback("too large");
    }
    let options = resvg::usvg::Options::default();
    let Ok(tree) = resvg::usvg::Tree::from_data_nested(svg, &options) else {
        return fallback(invalid_kind(kind));
    };
    let size = tree.size();
    if size.width() > AXIS_CAP || size.height() > AXIS_CAP {
        return fallback("too large");
    }
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return fallback(invalid_kind(kind));
    }
    let pixels = size.width() * size.height();
    // Widening float comparison against the pixel cap; NaN and infinity fall out here.
    if !pixels.is_finite() || f64::from(pixels) > f64::from(PIXEL_CAP) {
        return fallback("too large");
    }
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;
    if width == 0 || height == 0 {
        return fallback(invalid_kind(kind));
    }
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return fallback("too large");
    };
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    let Some(bytes) = encode_png(&pixmap) else {
        return fallback("too large");
    };
    if bytes.len() > PNG_CAP {
        return fallback("too large");
    }
    DiagramOutcome::Pixels(Arc::from(bytes.into_boxed_slice()))
}

pub(crate) fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24
        || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
        || bytes.get(12..16) != Some(b"IHDR")
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn invalid_kind(kind: super::DiagramKind) -> &'static str {
    match kind {
        super::DiagramKind::D2 => "invalid d2",
        super::DiagramKind::Nomnoml => "invalid nomnoml",
        super::DiagramKind::Dot => "invalid dot",
        super::DiagramKind::Mermaid => "invalid mermaid",
    }
}

/// Scales a pixel image into a cell box, returning column and row counts.
#[must_use]
pub fn scale_to_cells(
    image_width: u32,
    image_height: u32,
    cell_width: u32,
    cell_height: u32,
    max_columns: u32,
    max_rows: u32,
) -> (u32, u32) {
    if image_width == 0 || image_height == 0 || cell_width == 0 || cell_height == 0 {
        return (0, 0);
    }
    let box_width = u128::from(max_columns) * u128::from(cell_width);
    let box_height = u128::from(max_rows) * u128::from(cell_height);
    let image_width = u128::from(image_width);
    let image_height = u128::from(image_height);
    let cell_width = u128::from(cell_width);
    let cell_height = u128::from(cell_height);
    // Products of u32 pairs stay far below u128; every quotient below is
    // capped at its caller-supplied maximum, so the final narrowing cannot
    // lose a value.
    let (columns, rows) = if image_width <= box_width && image_height <= box_height {
        // The image already fits: keep its natural cell size, half up.
        (
            div_round(image_width, cell_width),
            div_round(image_height, cell_height),
        )
    } else if box_width * image_height <= box_height * image_width {
        // Width binds: columns land exactly on the cap while rows round up
        // so no source row is cropped.
        (
            u128::from(max_columns),
            ceil_div(image_height * box_width, image_width * cell_height).min(u128::from(max_rows)),
        )
    } else {
        (
            ceil_div(image_width * box_height, image_height * cell_width)
                .min(u128::from(max_columns)),
            u128::from(max_rows),
        )
    };
    (
        u32::try_from(columns).unwrap_or(u32::MAX).max(1),
        u32::try_from(rows).unwrap_or(u32::MAX).max(1),
    )
}

/// Integer division rounding half up; the divisor must be nonzero.
fn div_round(numerator: u128, denominator: u128) -> u128 {
    (numerator + denominator / 2) / denominator
}

/// Integer division rounding up; the divisor must be nonzero.
fn ceil_div(numerator: u128, denominator: u128) -> u128 {
    numerator.div_ceil(denominator)
}

fn encode_png(pixmap: &tiny_skia::Pixmap) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(pixmap.data()).ok()?;
    drop(writer);
    Some(bytes)
}

fn fallback(reason: &str) -> DiagramOutcome {
    DiagramOutcome::Fallback {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{png_dimensions, render_svg, scale_to_cells};
    use crate::diagram::{DiagramKind, DiagramOutcome};

    const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"64\" height=\"64\"><rect width=\"64\" height=\"64\"/></svg>";

    #[test]
    fn image_hrefs_never_reach_the_raster() {
        let with_href = SVG.replace("<rect", "<image href=\"file:///etc/nowhere\"/><rect");
        if let (DiagramOutcome::Pixels(left), DiagramOutcome::Pixels(right)) = (
            render_svg(SVG.as_bytes(), DiagramKind::D2),
            render_svg(with_href.as_bytes(), DiagramKind::D2),
        ) {
            assert_eq!(left, right);
        }
    }

    #[test]
    fn declared_size_cap_rejects_before_raster() {
        let wide = SVG.replace("width=\"64\"", "width=\"40000\"");
        assert!(matches!(
            render_svg(wide.as_bytes(), DiagramKind::D2),
            DiagramOutcome::Fallback { .. }
        ));
    }

    #[test]
    fn same_source_rasterizes_to_same_bytes() {
        if let (DiagramOutcome::Pixels(left), DiagramOutcome::Pixels(right)) = (
            render_svg(SVG.as_bytes(), DiagramKind::D2),
            render_svg(SVG.as_bytes(), DiagramKind::D2),
        ) {
            assert_eq!(left, right);
        }
    }

    #[test]
    fn cell_scaling_matches_the_pinned_math() {
        assert_eq!(scale_to_cells(1_920, 1_080, 10, 20, 40, 24), (40, 12));
        assert_eq!(scale_to_cells(100, 100, 10, 20, 40, 24), (10, 5));
        assert_eq!(scale_to_cells(1_080, 1_920, 20, 10, 24, 40), (12, 40));
    }
    #[test]
    fn png_dimensions_match_rasterized_svg() {
        let DiagramOutcome::Pixels(png) = render_svg(SVG.as_bytes(), DiagramKind::Dot) else {
            panic!("valid SVG must rasterize");
        };
        assert_eq!(png_dimensions(&png), Some((64, 64)));
        assert_eq!(png_dimensions(b"not a PNG"), None);
    }
}
