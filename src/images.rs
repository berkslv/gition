//! Decoded-image and terminal-protocol cache for inline images.
//! Paths are repo-relative. Without the `native` feature images are never
//! shown and the viewer falls back to placeholders.

#[cfg(not(feature = "native"))]
pub use stub::Images;

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
}

#[cfg(feature = "native")]
pub use native::Images;

#[cfg(feature = "native")]
mod native {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use image::DynamicImage;
    use ratatui::layout::Size;
    use ratatui_image::Resize;
    use ratatui_image::picker::Picker;
    use ratatui_image::protocol::Protocol;

    pub struct Images {
        root: PathBuf,
        picker: Option<Picker>,
        decoded: HashMap<PathBuf, Option<DynamicImage>>,
        protocols: HashMap<(PathBuf, Size), Protocol>,
    }

    impl Images {
        pub fn new(root: PathBuf, picker: Option<Picker>) -> Self {
            Self { root, picker, decoded: HashMap::new(), protocols: HashMap::new() }
        }

        pub fn enabled(&self) -> bool {
            self.picker.is_some()
        }

        fn decoded(&mut self, path: &Path) -> Option<&DynamicImage> {
            self.decoded
                .entry(path.to_path_buf())
                .or_insert_with(|| {
                    image::ImageReader::open(self.root.join(path)).ok()?.with_guessed_format().ok()?.decode().ok()
                })
                .as_ref()
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
            self.decoded.remove(path);
            self.protocols.retain(|(p, _), _| p != path);
        }
    }
}
