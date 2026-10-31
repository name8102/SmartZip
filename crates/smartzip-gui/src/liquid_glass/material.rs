//! Software backdrop sampling for the cross-platform Liquid Glass preview.

use image::{imageops, Rgba, RgbaImage};

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub struct Scene {
    background: RgbaImage,
    blurred: RgbaImage,
    dark: bool,
}

impl Scene {
    pub fn new(width: u32, height: u32, dark: bool, variant: u8) -> Self {
        let mut background = RgbaImage::new(width, height);
        let phase = f32::from(variant % 3) * 0.83;
        let base = if dark {
            [21.0, 28.0, 38.0]
        } else {
            [235.0, 241.0, 245.0]
        };
        let colors = if dark {
            [[37.0, 75.0, 88.0], [64.0, 60.0, 91.0], [34.0, 65.0, 81.0]]
        } else {
            [
                [162.0, 205.0, 211.0],
                [199.0, 190.0, 220.0],
                [182.0, 213.0, 226.0],
            ]
        };

        // Curves are evaluated per column; the pixel loop only needs distances
        // and smooth polynomial weights, not transcendental falloff functions.
        for x in 0..width {
            let u = x as f32 / width as f32;
            let centers = [
                0.24 + 0.15 * (u * 4.5 + phase).sin() + u * 0.16,
                0.69 + 0.17 * (u * 3.8 + phase + 1.6).sin() - u * 0.12,
                0.94 - u * 0.56 + 0.09 * (u * 6.0 + phase).sin(),
            ];
            for y in 0..height {
                let v = y as f32 / height as f32;
                let mut color = base;
                for (index, center) in centers.iter().enumerate() {
                    let distance = (v - center).abs();
                    let weight = (1.0 - smoothstep(0.015, 0.23, distance)) * 0.64;
                    color = mix(color, colors[index], weight);
                    // A pair of quiet contour lines gives the lens something
                    // legible to bend without turning the backdrop into noise.
                    for offset in [-0.057_f32, 0.069] {
                        let line = 1.0
                            - smoothstep(0.45, 1.65, (v - center - offset).abs() * height as f32);
                        let ink = if dark { 13.0 } else { -19.0 };
                        for channel in &mut color {
                            *channel += ink * line;
                        }
                    }
                }
                let light = (1.0 - v) * if dark { 3.0 } else { 5.0 };
                for channel in &mut color {
                    *channel += light;
                }
                background.put_pixel(x, y, opaque(color));
            }
        }
        let blur_radius = (width.min(height) as f32 * 0.014).clamp(2.0, 12.0);
        let blurred = imageops::blur(&background, blur_radius);
        Self {
            background,
            blurred,
            dark,
        }
    }

    pub fn background(&self) -> &RgbaImage {
        &self.background
    }

    pub fn refract(&self, rect: Rect, radius: f32, tint: f32) -> RgbaImage {
        let width = rect.width.round().max(1.0) as u32;
        let height = rect.height.round().max(1.0) as u32;
        let half = [width as f32 * 0.5, height as f32 * 0.5];
        let radius = radius.clamp(0.0, half[0].min(half[1]));
        let edge_width = (radius * 0.7).clamp(8.0, 24.0);
        let tint_color = if self.dark {
            [25.0, 31.0, 42.0]
        } else {
            [248.0, 250.0, 253.0]
        };
        let tint = tint.clamp(0.0, 1.0);

        RgbaImage::from_fn(width, height, |x, y| {
            let point = [x as f32 + 0.5 - half[0], y as f32 + 0.5 - half[1]];
            let (distance, normal) = rounded_rect_distance(point, half, radius);
            let depth = (-distance).max(0.0);
            let lens = 1.0 - smoothstep(0.0, edge_width, depth);
            let displacement = lens * lens * edge_width * 0.48;
            let sample_x = rect.x + x as f32 + normal[0] * displacement;
            let sample_y = rect.y + y as f32 + normal[1] * displacement;
            let soft = sample(&self.blurred, sample_x, sample_y);
            let clear = sample(&self.background, sample_x, sample_y);
            let mut color = mix(soft, clear, lens * 0.72);
            color = mix(color, tint_color, tint);

            // A narrow directional rim accompanies the actual displaced image;
            // the rim is not a substitute for backdrop sampling.
            let rim = 1.0 - smoothstep(0.25, 1.8, depth);
            let inner_rim = (1.0 - smoothstep(1.0, 3.4, depth)) * (1.0 - rim);
            let light = (-normal[0] * 0.48 - normal[1] * 0.72).clamp(-1.0, 1.0);
            let highlight = rim * (light.max(0.0) * 24.0 + 3.0);
            let shadow = inner_rim * (1.0 - light) * if self.dark { 4.0 } else { 7.0 };
            for channel in &mut color {
                *channel += highlight - shadow;
            }
            opaque(color)
        })
    }
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    std::array::from_fn(|index| from[index] + (to[index] - from[index]) * amount)
}

fn opaque(color: [f32; 3]) -> Rgba<u8> {
    Rgba([
        color[0].round().clamp(0.0, 255.0) as u8,
        color[1].round().clamp(0.0, 255.0) as u8,
        color[2].round().clamp(0.0, 255.0) as u8,
        255,
    ])
}

fn sample(image: &RgbaImage, x: f32, y: f32) -> [f32; 3] {
    let x = x.clamp(0.0, (image.width() - 1) as f32);
    let y = y.clamp(0.0, (image.height() - 1) as f32);
    let left = x.floor() as u32;
    let top = y.floor() as u32;
    let right = (left + 1).min(image.width() - 1);
    let bottom = (top + 1).min(image.height() - 1);
    let pixel = |x, y| {
        let pixel = image.get_pixel(x, y);
        [
            f32::from(pixel[0]),
            f32::from(pixel[1]),
            f32::from(pixel[2]),
        ]
    };
    mix(
        mix(pixel(left, top), pixel(right, top), x.fract()),
        mix(pixel(left, bottom), pixel(right, bottom), x.fract()),
        y.fract(),
    )
}

/// Signed distance and outward normal of a centered rounded rectangle.
fn rounded_rect_distance(point: [f32; 2], half: [f32; 2], radius: f32) -> (f32, [f32; 2]) {
    let q = [
        point[0].abs() - half[0] + radius,
        point[1].abs() - half[1] + radius,
    ];
    let outside = [q[0].max(0.0), q[1].max(0.0)];
    let length = outside[0].hypot(outside[1]);
    let normal = if length > 0.0 {
        [outside[0] / length, outside[1] / length]
    } else if q[0] > q[1] {
        [1.0, 0.0]
    } else {
        [0.0, 1.0]
    };
    (
        length + q[0].max(q[1]).min(0.0) - radius,
        [normal[0] * point[0].signum(), normal[1] * point[1].signum()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refraction_changes_the_backdrop_without_tint() {
        let scene = Scene::new(120, 90, false, 0);
        let rect = Rect {
            x: 12.0,
            y: 10.0,
            width: 70.0,
            height: 60.0,
        };
        let glass = scene.refract(rect, 14.0, 0.0);
        let changed = glass
            .enumerate_pixels()
            .filter(|(x, y, pixel)| **pixel != *scene.background().get_pixel(x + 12, y + 10))
            .count();
        assert!(changed > (glass.width() * glass.height() / 4) as usize);
    }

    #[test]
    fn edge_displaces_the_source_not_just_its_brightness() {
        let background = RgbaImage::from_fn(80, 80, |x, y| {
            Rgba([(x * 2) as u8, (y * 2) as u8, 100, 255])
        });
        let scene = Scene {
            blurred: background.clone(),
            background,
            dark: false,
        };
        let glass = scene.refract(
            Rect {
                x: 20.,
                y: 20.,
                width: 40.,
                height: 40.,
            },
            12.,
            0.,
        );
        // Equal-channel highlights cannot change red minus green. On the left
        // rim, the gradient must be sampled to the left of the unshifted x=21.
        let rim = glass.get_pixel(1, 20);
        assert!(i16::from(rim[0]) - i16::from(rim[1]) < 42 - 80 - 4);
        assert_eq!(*glass.get_pixel(20, 20), Rgba([80, 80, 100, 255]));
    }

    #[test]
    fn themes_and_variants_have_distinct_backdrops() {
        let light = Scene::new(48, 36, false, 0);
        let dark = Scene::new(48, 36, true, 0);
        let shifted = Scene::new(48, 36, false, 1);
        assert_ne!(light.background(), dark.background());
        assert_ne!(light.background(), shifted.background());
    }

    #[test]
    fn out_of_bounds_sampling_is_clamped_and_opaque() {
        let scene = Scene::new(1, 1, true, 2);
        let glass = scene.refract(
            Rect {
                x: -200.0,
                y: 200.0,
                width: 13.4,
                height: 9.8,
            },
            50.0,
            0.25,
        );
        assert_eq!(glass.dimensions(), (13, 10));
        assert!(glass.pixels().all(|pixel| pixel[3] == 255));
        assert!(scene.background().pixels().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn rounded_corner_normal_points_diagonally_outward() {
        let (distance, normal) = rounded_rect_distance([48.0, 48.0], [50.0, 50.0], 12.0);
        assert!(distance > 0.0);
        assert!((normal[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.0001);
        assert!((normal[1] - normal[0]).abs() < 0.0001);
    }
}
