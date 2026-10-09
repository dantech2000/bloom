// SPDX-License-Identifier: AGPL-3.0-or-later
//! The colours of a picture, for a light: the one colour of an artwork,
//! and the bands of a picture from its top to its bottom.

/// The colour a light can take from a picture: the strongest hue of it,
/// at full strength (the brightness of the light is its own setting).
/// None for a picture with no colour to speak of (grey, black, white).
pub fn artwork_colour(picture: &image::RgbaImage) -> Option<[u8; 3]> {
    /// Hues are counted in ranges of 15 degrees.
    const RANGES: usize = 24;
    // Some thousand pixels say as much as all of them.
    let step = ((picture.width() as usize * picture.height() as usize) / 8000).max(1);
    let mut ranges = [(0f64, [0f64; 3]); RANGES];
    let mut seen = 0usize;
    for pixel in picture.pixels().step_by(step) {
        let [r, g, b, a] = pixel.0.map(f64::from);
        if a < 128. {
            continue;
        }
        seen += 1;
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        if max < 50. || (max - min) / max < 0.25 {
            // Too dark or too grey to have a hue.
            continue;
        }
        let spread = max - min;
        let hue = 60.
            * if max == r {
                ((g - b) / spread).rem_euclid(6.)
            } else if max == g {
                (b - r) / spread + 2.
            } else {
                (r - g) / spread + 4.
            };
        // A strong, bright pixel counts for more than a dull one.
        let weight = (spread / max) * (max / 255.);
        let range = &mut ranges[(hue / 15.) as usize % RANGES];
        range.0 += weight;
        for (sum, part) in range.1.iter_mut().zip([r, g, b]) {
            *sum += part * weight;
        }
    }
    let best = (0..RANGES).max_by(|a, b| ranges[*a].0.total_cmp(&ranges[*b].0))?;
    // A few coloured pixels in a grey picture are not its colour.
    // A picture with no pixel to see has none either.
    if ranges[best].0 <= 0. || ranges[best].0 < seen as f64 * 0.01 {
        return None;
    }
    let [r, g, b] = ranges[best].1.map(|sum| sum / ranges[best].0);
    // Full strength: a light shows a pale colour as white, so most of the
    // grey in the colour goes, and the strongest part becomes 255.
    let grey = r.min(g).min(b) * 0.85;
    let top = r.max(g).max(b) - grey;
    Some([r, g, b].map(|part| ((part - grey) / top * 255.).round().clamp(0., 255.) as u8))
}

/// The mean colour of each of `bands` bands of a picture, from its top to
/// its bottom: the artwork of a detail page, as `frame.colours` tells a
/// frame of a film.
pub fn picture_bands(picture: &image::RgbaImage, bands: usize) -> Vec<[u8; 3]> {
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    (0..bands)
        .map(|band| {
            let (from, to) = (height * band / bands, (height * (band + 1) / bands).max(height * band / bands + 1));
            let mut sum = [0u64; 3];
            let mut count = 0u64;
            for y in (from..to.min(height)).step_by(((to - from) / 8).max(1)) {
                for x in (0..width).step_by((width / 24).max(1)) {
                    let pixel = picture.get_pixel(x as u32, y as u32).0;
                    sum = [0, 1, 2].map(|part| sum[part] + u64::from(pixel[part]));
                    count += 1;
                }
            }
            sum.map(|part| (part / count.max(1)) as u8)
        })
        .collect()
}

/// A hue in degrees as a colour at full strength.
pub(super) fn hue_colour(hue: f32) -> [u8; 3] {
    let part = |shift: f32| {
        let k = (shift + hue / 60.).rem_euclid(6.);
        (255. * (1. - k.min(4. - k).clamp(0., 1.))).round() as u8
    };
    [part(5.), part(3.), part(1.)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour of a picture is its strongest hue at full strength; a
    /// picture of greys has none.
    #[test]
    fn the_colour_of_a_picture_is_its_strongest_hue() {
        let picture = |pixel: &dyn Fn(u32, u32) -> [u8; 3]| {
            image::RgbaImage::from_fn(60, 90, |x, y| {
                let [r, g, b] = pixel(x, y);
                image::Rgba([r, g, b, 255])
            })
        };
        // A dark poster with a band of dull orange and a little blue.
        let poster = picture(&|x, y| match (x, y) {
            (_, 30..50) => [150, 80, 30],
            (0..6, 0..6) => [20, 40, 200],
            _ => [12, 12, 14],
        });
        let [r, g, b] = artwork_colour(&poster).expect("no colour");
        assert!(r == 255 && (90..140).contains(&g) && b < 30, "not a strong orange: {r}, {g}, {b}");
        // Greys, black and white have no hue.
        assert_eq!(artwork_colour(&picture(&|x, _| [x as u8 * 4; 3])), None);
        // A few coloured pixels in a grey picture are not its colour.
        assert_eq!(artwork_colour(&picture(&|x, y| if (x, y) == (0, 0) { [255, 0, 0] } else { [90; 3] })), None);
        // The bands of a picture: its top first.
        let halves = picture(&|_, y| if y < 45 { [200, 0, 0] } else { [0, 0, 100] });
        assert_eq!(picture_bands(&halves, 2), [[200, 0, 0], [0, 0, 100]]);
        // Nothing to see: a clear picture, and one with no size.
        assert_eq!(artwork_colour(&image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 0]))), None);
        assert_eq!(artwork_colour(&image::RgbaImage::new(0, 0)), None);
        assert_eq!(picture_bands(&image::RgbaImage::new(0, 0), 3), [[0; 3]; 3]);
        assert_eq!(picture_bands(&picture(&|_, _| [9, 9, 9]), 200).len(), 200);
        assert_eq!(hue_colour(0.), [255, 0, 0]);
        assert_eq!(hue_colour(120.), [0, 255, 0]);
        assert_eq!(hue_colour(240.), [0, 0, 255]);
        assert_eq!(hue_colour(300.), [255, 0, 255]);
    }
}
