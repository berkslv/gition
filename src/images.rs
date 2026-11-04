//! Decoded-image and terminal-protocol cache for inline images.
//! Paths are repo-relative. Without the `native` feature the frontend loads the images: the
//! viewer leaves a [`SLOT`] in the grid for each one and the frontend shows the image there.

#[cfg(not(feature = "native"))]
pub use web::{Images, SLOT, compress, compressible};

#[cfg(not(feature = "native"))]
mod web {
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    use ratatui::layout::{Rect, Size};
    use ratatui::style::Color;

    /// Background of the cells an image covers. The frontend draws the image under the grid
    /// and makes these cells transparent; a popup drawn over them resets them, and hides it.
    pub const SLOT: Color = Color::Rgb(1, 2, 3);

    /// Image sizes the frontend measured, and where the last frame showed each image.
    #[derive(Default)]
    pub struct Images {
        /// Cell size in pixels. Images show once the frontend sets it.
        cell: Option<(f32, f32)>,
        /// Pixel size of each image, or `None` if it couldn't be loaded.
        sizes: HashMap<PathBuf, Option<(u32, u32)>>,
        /// Images the viewer asked for whose size isn't known yet, and whether the frontend was told.
        pending: BTreeMap<PathBuf, bool>,
        shown: Vec<(PathBuf, Rect)>,
    }

    impl Images {
        pub fn enabled(&self) -> bool {
            self.cell.is_some()
        }

        /// Cell size the image takes when fitted into `max`, like `ratatui_image`'s
        /// `Resize::Fit`: never larger than the image. `None` until the frontend loaded it.
        pub fn fit(&mut self, path: &Path, max: Size) -> Option<Size> {
            let (cw, ch) = self.cell?;
            let Some(&size) = self.sizes.get(path) else {
                self.pending.entry(path.to_path_buf()).or_default();
                return None;
            };
            let (w, h) = size?;
            let (w, h) = (w as f32, h as f32);
            let scale = (f32::from(max.width) * cw / w).min(f32::from(max.height) * ch / h).min(1.0);
            let cells = |px: f32, cell: f32, max: u16| ((px * scale / cell).ceil() as u16).min(max);
            let size = Size::new(cells(w, cw, max.width), cells(h, ch, max.height));
            (size.width > 0 && size.height > 0).then_some(size)
        }

        pub fn forget(&mut self, path: &Path) {
            self.sizes.remove(path);
        }

        /// Records where the viewer put an image this frame.
        pub fn show(&mut self, path: &Path, area: Rect) {
            self.shown.push((path.to_path_buf(), area));
        }

        // ---- for the frontend ----

        /// Sets the cell size in pixels; true if it changed, and the page needs laying out again.
        pub fn set_cell_size(&mut self, cell: (f32, f32)) -> bool {
            self.cell.replace(cell) != Some(cell)
        }

        /// Images to load, each handed out once.
        pub fn take_wanted(&mut self) -> Vec<PathBuf> {
            self.pending
                .iter_mut()
                .filter(|(_, told)| !**told)
                .map(|(p, told)| {
                    *told = true;
                    p.clone()
                })
                .collect()
        }

        /// An image's pixel size, or `None` if it can't be shown.
        pub fn loaded(&mut self, path: PathBuf, size: Option<(u32, u32)>) {
            self.pending.remove(&path);
            self.sizes.insert(path, size);
        }

        /// Where the last frame showed images.
        pub fn take_shown(&mut self) -> Vec<(PathBuf, Rect)> {
            std::mem::take(&mut self.shown)
        }
    }

    pub fn compressible(_bytes: &[u8]) -> bool {
        false
    }

    pub fn compress(_bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fits_without_growing() {
            let mut images = Images::default();
            let p = Path::new("a.png");
            assert_eq!(images.fit(p, Size::new(80, 20)), None);
            images.set_cell_size((10.0, 20.0));
            assert_eq!(images.fit(p, Size::new(80, 20)), None);
            assert_eq!(images.take_wanted(), vec![p.to_path_buf()]);
            assert!(images.take_wanted().is_empty());
            images.loaded(p.into(), Some((200, 100)));
            assert_eq!(images.fit(p, Size::new(80, 20)), Some(Size::new(20, 5)));
            // Twice the pane's width: halved.
            images.loaded(p.into(), Some((1600, 200)));
            assert_eq!(images.fit(p, Size::new(80, 20)), Some(Size::new(80, 5)));
            images.loaded(p.into(), None);
            assert_eq!(images.fit(p, Size::new(80, 20)), None);
        }
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
