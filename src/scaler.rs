// SPDX-License-Identifier: MPL-2.0

//! Background scaling methods such as fit, stretch, and zoom.

use cosmic_bg_config::FilterMethod;
use image::DynamicImage;

fn to_resize_alg(filter: &FilterMethod) -> fast_image_resize::ResizeAlg {
    match filter {
        FilterMethod::Nearest => fast_image_resize::ResizeAlg::Nearest,
        FilterMethod::Linear => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear)
        }
        FilterMethod::Lanczos => {
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3)
        }
    }
}

pub fn fit(
    img: &image::DynamicImage,
    color: &[f32; 3],
    layer_width: u32,
    layer_height: u32,
    filter: &FilterMethod,
) -> image::DynamicImage {
    if layer_width == 0 || layer_height == 0 || img.width() == 0 || img.height() == 0 {
        let r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        let g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        let b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        return DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            layer_width,
            layer_height,
            image::Rgb([r, g, b]),
        ));
    }

    let r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;

    let mut filled_image =
        image::RgbImage::from_pixel(layer_width, layer_height, image::Rgb([r, g, b]));

    let (w, h) = (img.width(), img.height());
    let ratio = (layer_width as f64 / w as f64).min(layer_height as f64 / h as f64);
    let new_width = ((w as f64 * ratio).round() as u32).max(1);
    let new_height = ((h as f64 * ratio).round() as u32).max(1);

    let resized_image = resize(img, new_width, new_height, filter);
    let resized_rgb = resized_image.to_rgb8();

    image::imageops::replace(
        &mut filled_image,
        &resized_rgb,
        ((layer_width - new_width) / 2) as i64,
        ((layer_height - new_height) / 2) as i64,
    );

    DynamicImage::ImageRgb8(filled_image)
}

pub fn stretch(
    img: &image::DynamicImage,
    layer_width: u32,
    layer_height: u32,
    filter: &FilterMethod,
) -> image::DynamicImage {
    resize(img, layer_width, layer_height, filter)
}

pub fn zoom(
    img: &image::DynamicImage,
    layer_width: u32,
    layer_height: u32,
    filter: &FilterMethod,
) -> image::DynamicImage {
    if layer_width == 0 || layer_height == 0 || img.width() == 0 || img.height() == 0 {
        return DynamicImage::new(layer_width, layer_height, img.color());
    }

    let mut resizer = fast_image_resize::Resizer::new();
    let options = fast_image_resize::ResizeOptions {
        algorithm: to_resize_alg(filter),
        cropping: fast_image_resize::SrcCropping::FitIntoDestination((0.5, 0.5)),
        ..Default::default()
    };
    let mut new_image = DynamicImage::new(layer_width, layer_height, img.color());
    if let Err(err) = resizer.resize(img, &mut new_image, &options) {
        tracing::warn!(?err, "Failed to use `fast_image_resize`. Falling back.");
        // Centered source crop before resizing to avoid intermediate memory explosion
        let (w, h) = (img.width(), img.height());
        let src_ratio = w as f64 / h as f64;
        let dst_ratio = layer_width as f64 / layer_height as f64;
        let (crop_w, crop_h) = if src_ratio > dst_ratio {
            let crop_w = (h as f64 * dst_ratio).round() as u32;
            (crop_w.clamp(1, w), h)
        } else {
            let crop_h = (w as f64 / dst_ratio).round() as u32;
            (w, crop_h.clamp(1, h))
        };
        let crop_x = (w - crop_w) / 2;
        let crop_y = (h - crop_h) / 2;
        let cropped = img.crop_imm(crop_x, crop_y, crop_w, crop_h);
        new_image =
            image::imageops::resize(&cropped, layer_width, layer_height, (*filter).into()).into();
    }
    new_image
}

fn resize(
    img: &image::DynamicImage,
    new_width: u32,
    new_height: u32,
    filter: &FilterMethod,
) -> image::DynamicImage {
    if new_width == 0 || new_height == 0 || img.width() == 0 || img.height() == 0 {
        return DynamicImage::new(new_width, new_height, img.color());
    }
    let mut resizer = fast_image_resize::Resizer::new();
    let options = fast_image_resize::ResizeOptions {
        algorithm: to_resize_alg(filter),
        ..Default::default()
    };
    let mut new_image = DynamicImage::new(new_width, new_height, img.color());
    if let Err(err) = resizer.resize(img, &mut new_image, &options) {
        tracing::warn!(?err, "Failed to use `fast_image_resize`. Falling back.");
        new_image = image::imageops::resize(img, new_width, new_height, (*filter).into()).into();
    }
    new_image
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgb, RgbImage};

    #[test]
    fn test_zoom_extreme_aspect_ratio_bounded_size() {
        // A tall thin image: 100 x 2000
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 2000, Rgb([10, 20, 30])));
        let dst = zoom(&src, 384, 216, &FilterMethod::Lanczos);

        assert_eq!(dst.width(), 384);
        assert_eq!(dst.height(), 216);
    }

    #[test]
    fn test_fit_letterboxing_8bit() {
        // A 100x50 image in a 100x100 layer with green background
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 50, Rgb([0, 0, 255])));
        let bg_color = [0.0, 1.0, 0.0];
        let dst = fit(&src, &bg_color, 100, 100, &FilterMethod::Linear);

        assert_eq!(dst.width(), 100);
        assert_eq!(dst.height(), 100);

        // Top border should be green background
        let top_pixel = dst.get_pixel(50, 5);
        assert_eq!(top_pixel[0], 0);
        assert_eq!(top_pixel[1], 255);
        assert_eq!(top_pixel[2], 0);

        // Center should be blue foreground
        let center_pixel = dst.get_pixel(50, 50);
        assert_eq!(center_pixel[0], 0);
        assert_eq!(center_pixel[1], 0);
        assert_eq!(center_pixel[2], 255);
    }

    #[test]
    fn test_stretch() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(50, 50, Rgb([100, 150, 200])));
        let dst = stretch(&src, 200, 100, &FilterMethod::Nearest);

        assert_eq!(dst.width(), 200);
        assert_eq!(dst.height(), 100);
    }
}
