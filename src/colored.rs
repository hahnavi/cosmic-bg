// SPDX-License-Identifier: MPL-2.0

use colorgrad::Gradient as ColorGradientTrait;
use cosmic_bg_config::Gradient;
use image::RgbImage;
use rayon::prelude::*;

/// Generate a 1x1 background image from a color.
pub fn single_1x1(color: [f32; 3]) -> RgbImage {
    let r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;
    image::ImageBuffer::from_pixel(1, 1, image::Rgb([r, g, b]))
}

/// Generate a background image from a color with given dimensions.
#[allow(dead_code)]
pub fn single(color: [f32; 3], width: u32, height: u32) -> RgbImage {
    let r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;
    image::ImageBuffer::from_pixel(width, height, image::Rgb([r, g, b]))
}

/// Generate a background image from a gradient.
pub fn gradient(
    gradient: &Gradient,
    width: u32,
    height: u32,
) -> Result<RgbImage, colorgrad::GradientBuilderError> {
    let mut colors = Vec::with_capacity(gradient.colors.len());
    for &[r, g, b] in &*gradient.colors {
        colors.push(colorgrad::Color::from_linear_rgba(r, g, b, 1.0));
    }

    let grad = colorgrad::GradientBuilder::new()
        .colors(&colors)
        .mode(colorgrad::BlendMode::LinearRgb)
        .build::<colorgrad::LinearGradient>()?;

    let (dmin, dmax) = grad.domain();
    let angle = gradient.radius.to_radians();
    // Angle convention: 0 is to-top (decreasing y), 90 is to-right (increasing x),
    // 180 is to-bottom (increasing y), 270 is to-left (decreasing x).
    let ux = angle.sin();
    let uy = -angle.cos();
    let max_x = (width.saturating_sub(1)) as f32;
    let max_y = (height.saturating_sub(1)) as f32;

    // Project the 4 corners of the image onto the direction vector
    let p1: f32 = 0.0;
    let p2 = max_x * ux;
    let p3 = max_x * ux + max_y * uy;
    let p4 = max_y * uy;

    let pmin = p1.min(p2).min(p3).min(p4);
    let pmax = p1.max(p2).max(p3).max(p4);
    let range = pmax - pmin;

    let mut imgbuf = image::RgbImage::new(width, height);
    imgbuf.par_enumerate_pixels_mut().for_each(|(x, y, pixel)| {
        let x_f = x as f32;
        let y_f = y as f32;
        let proj = x_f * ux + y_f * uy;
        let t = if range > f32::EPSILON {
            (proj - pmin) / range
        } else {
            0.0
        };
        let pos = dmin + t * (dmax - dmin);
        let [r, g, b, _] = grad.at(pos).to_rgba8();
        *pixel = image::Rgb([r, g, b]);
    });

    Ok(imgbuf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    static BW_COLORS: [[f32; 3]; 2] = [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];

    #[test]
    fn test_single_1x1() {
        let img = single_1x1([1.0, 0.5, 0.0]);
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
        let p = img.get_pixel(0, 0);
        assert_eq!(p.0, [255, 128, 0]);
    }

    #[test]
    fn test_gradient_45_deg_covers_full_range() {
        let grad_cfg = Gradient {
            colors: Cow::Borrowed(&BW_COLORS),
            radius: 45.0,
        };

        let img = gradient(&grad_cfg, 100, 100).unwrap();
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);

        let mut min_val = 255u8;
        let mut max_val = 0u8;
        for p in img.pixels() {
            min_val = min_val.min(p[0]);
            max_val = max_val.max(p[0]);
        }

        // Full range from black to white should be represented
        assert_eq!(min_val, 0, "45-degree gradient should reach black");
        assert_eq!(max_val, 255, "45-degree gradient should reach white");
    }

    #[test]
    fn test_gradient_cardinal_and_continuity() {
        let grad_cfg_90 = Gradient {
            colors: Cow::Borrowed(&BW_COLORS),
            radius: 90.0,
        };
        let img_90 = gradient(&grad_cfg_90, 100, 10).unwrap();
        // At 90 degrees, x=0 should be 0, x=99 should be near 255
        assert_eq!(img_90.get_pixel(0, 5)[0], 0);
        assert_eq!(img_90.get_pixel(99, 5)[0], 255);

        // Near cardinal: 89.9 deg and 90.1 deg should have almost identical values to 90 deg
        let grad_cfg_89 = Gradient {
            colors: Cow::Borrowed(&BW_COLORS),
            radius: 89.9,
        };
        let img_89 = gradient(&grad_cfg_89, 100, 10).unwrap();
        let diff = (img_90.get_pixel(50, 5)[0] as i32 - img_89.get_pixel(50, 5)[0] as i32).abs();
        assert!(
            diff <= 1,
            "Discontinuity detected near 90 degrees: diff={diff}"
        );
    }
}
