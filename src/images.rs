//! Decoded-image and terminal-protocol cache for inline images.
//! Paths are repo-relative. Without the `native` feature images are never
//! shown and the viewer falls back to placeholders.

#[cfg(not(feature = "native"))]
pub use stub::{Images, compress, compressible};

#[cfg(not(feature = "native"))]
mod stub {
    use ratatui::layout::Size;
    use std::path::Path;

    #[derive(Default)]
    pub struct Images {
        _private: (),
    }

    impl Images {
        pub fn enabled(&self) -> bool {
            false
        }
        pub fn fit(&mut self, _path: &Path, _max: Size) -> Option<Size> {
            None
        }
        pub fn forget(&mut self, _path: &Path) {}
    }

    pub fn compressible(_bytes: &[u8]) -> bool {
        false
    }

    pub fn compress(_bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(feature = "native")]
pub use native::{Images, compress, compressible};

#[cfg(feature = "native")]
mod native {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use image::DynamicImage;
    use ratatui::layout::Size;
    use ratatui_image::Resize;
    use ratatui_image::picker::Picker;
    use ratatui_image::protocol::Protocol;

    /// Images are kept at most this many cells across and down, far more than a pane shows.
    /// Fitting the smaller copy gives the same cell size as fitting the original.
    const MAX_CELLS: (u32, u32) = (512, 64);
    /// Decoded pixels kept across pages before the cache starts over.
    const CACHE_BYTES: usize = 256 << 20;

    pub struct Images {
        root: PathBuf,
        picker: Option<Picker>,
        decoded: HashMap<PathBuf, Option<DynamicImage>>,
        decoded_bytes: usize,
        protocols: HashMap<(PathBuf, Size), Protocol>,
    }

    impl Images {
        pub fn new(root: PathBuf, picker: Option<Picker>) -> Self {
            Self { root, picker, decoded: HashMap::new(), decoded_bytes: 0, protocols: HashMap::new() }
        }

        pub fn enabled(&self) -> bool {
            self.picker.is_some()
        }

        fn decoded(&mut self, path: &Path) -> Option<&DynamicImage> {
            if !self.decoded.contains_key(path) {
                let img = self.decode(path);
                let bytes = img.as_ref().map_or(0, |i| i.as_bytes().len());
                if self.decoded_bytes + bytes > CACHE_BYTES {
                    self.decoded.clear();
                    self.protocols.clear();
                    self.decoded_bytes = 0;
                }
                self.decoded_bytes += bytes;
                self.decoded.insert(path.to_path_buf(), img);
            }
            self.decoded.get(path)?.as_ref()
        }

        /// Reads an image from the repo, which may not be the user's own: the decoder's memory is
        /// limited, and the result shrunk to what a terminal can show.
        fn decode(&self, path: &Path) -> Option<DynamicImage> {
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(16384);
            limits.max_image_height = Some(16384);
            limits.max_alloc = Some(256 << 20);
            let mut reader = image::ImageReader::open(self.root.join(path)).ok()?.with_guessed_format().ok()?;
            reader.limits(limits);
            let img = reader.decode().ok()?;
            let font = self.picker.as_ref()?.font_size();
            let (w, h) = (MAX_CELLS.0 * u32::from(font.width), MAX_CELLS.1 * u32::from(font.height));
            Some(if img.width() > w || img.height() > h { img.thumbnail(w, h) } else { img })
        }

        /// Cell size the image takes when fitted into `max`, or `None` if it can't be shown.
        pub fn fit(&mut self, path: &Path, max: Size) -> Option<Size> {
            let font = self.picker.as_ref()?.font_size();
            let img = self.decoded(path)?;
            let size = Resize::Fit(None).size_for(img, font, max);
            (size.width > 0 && size.height > 0).then_some(size)
        }

        pub fn protocol(&mut self, path: &Path, size: Size) -> Option<&Protocol> {
            let key = (path.to_path_buf(), size);
            if !self.protocols.contains_key(&key) {
                let img = self.decoded(path)?.clone();
                let proto = self.picker.as_ref()?.new_protocol(img, size, Resize::Fit(None)).ok()?;
                self.protocols.insert(key.clone(), proto);
            }
            self.protocols.get(&key)
        }

        /// Drops cached data for a file that may have changed on disk.
        pub fn forget(&mut self, path: &Path) {
            if let Some(Some(img)) = self.decoded.remove(path) {
                self.decoded_bytes -= img.as_bytes().len();
            }
            self.protocols.retain(|(p, _), _| p != path);
        }
    }

    /// Lossy WebP quality, 0-100.
    const QUALITY: f32 = 80.0;

    /// Whether [`compress`] would try `bytes`: a PNG or JPEG whose header reads. Cheap.
    pub fn compressible(bytes: &[u8]) -> bool {
        use image::ImageFormat::{Jpeg, Png};
        let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() else {
            return false;
        };
        matches!(reader.format(), Some(Png | Jpeg)) && reader.into_dimensions().is_ok()
    }

    /// Re-encodes a PNG or JPEG as lossy WebP, or `None` if it's another format, can't be
    /// decoded, or wouldn't get smaller.
    pub fn compress(bytes: &[u8]) -> Option<Vec<u8>> {
        use image::ImageDecoder;
        if !compressible(bytes) {
            return None;
        }
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        // WebP keeps no EXIF, so a phone photo's rotation is applied to the pixels.
        let orientation = decoder.orientation().ok()?;
        let mut img = DynamicImage::from_decoder(decoder).ok()?;
        img.apply_orientation(orientation);
        let (w, h) = (img.width(), img.height());
        let (pixels, layout) = if img.color().has_alpha() {
            (img.into_rgba8().into_raw(), webp::PixelLayout::Rgba)
        } else {
            (img.into_rgb8().into_raw(), webp::PixelLayout::Rgb)
        };
        let mut config = webp::WebPConfig::new().ok()?;
        config.quality = QUALITY;
        // Slowest, smallest.
        config.method = 6;
        let out = webp::Encoder::new(&pixels, layout, w, h).encode_advanced(&config).ok()?;
        (out.len() < bytes.len()).then(|| out.to_vec())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ratatui_image::FontSize;

        #[test]
        fn shrinking_keeps_the_fitted_size() {
            let font = FontSize::new(10, 20);
            let (w, h) = (MAX_CELLS.0 * 10, MAX_CELLS.1 * 20);
            for (iw, ih) in [(12000, 9000), (800, 30000), (30000, 400), (6000, 1300), (300, 200)] {
                let img = DynamicImage::new_rgb8(iw, ih);
                let small = if iw > w || ih > h { img.thumbnail(w, h) } else { img.clone() };
                for max in [Size::new(80, 20), Size::new(300, 20), Size::new(40, 10), Size::new(500, 64)] {
                    let a = Resize::Fit(None).size_for(&img, font, max);
                    let b = Resize::Fit(None).size_for(&small, font, max);
                    // Rounding may differ by a cell.
                    assert!(
                        a.width.abs_diff(b.width) <= 1 && a.height.abs_diff(b.height) <= 1,
                        "{iw}x{ih} in {max:?}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }
}
