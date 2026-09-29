//! Images attached to a prompt: pasted into the chat input, dropped onto it,
//! or chosen with its Attach images button. Only PNG, JPEG, GIF, and WebP
//! images of up to 5 MB are attached. Once the prompt is sent or queued, each
//! is saved in the project's data, under `images/`, named by the second it
//! was saved and a short hash of its content, so the same image attached
//! twice is saved once; the hidden anchor keeps their paths, and the harness
//! is given the images themselves (see [`crate::harness`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sha2::{Digest as _, Sha256};

use crate::hidden_anchor::APP_DIR;

/// The largest image that can be attached, in bytes.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Where the project's attached images are saved, in its data.
const IMAGES_DIR: &str = "images";

/// How many hex digits of an image's hash its saved name holds.
const HASH_DIGITS: usize = 12;

/// What an attached image is called when it was pasted rather than a file.
pub const PASTED: &str = "Pasted image";

/// An image attached to the prompt being written, held until it is sent.
#[derive(Clone, Debug, PartialEq)]
pub struct AttachedImage {
    /// Its bytes and format, which also render its thumbnail.
    pub image: Arc<Image>,
    /// Its file's name; none for one pasted.
    pub name: Option<String>,
    /// Its size in pixels.
    pub width: usize,
    pub height: usize,
}

/// Why an image wasn't attached, as a notification says it.
#[derive(Clone, Debug, PartialEq)]
pub enum Rejected {
    /// It isn't a PNG, JPEG, GIF, or WebP image.
    NotAnImage(String),
    /// It is larger than [`MAX_BYTES`].
    TooLarge(String),
    /// It couldn't be read.
    Unreadable(String, String),
}

impl Rejected {
    /// What the notification says.
    pub fn message(&self) -> String {
        match self {
            Self::NotAnImage(name) => format!(
                "{name} isn't an image: only PNG, JPEG, GIF, and WebP images can be attached."
            ),
            Self::TooLarge(name) => {
                format!("{name} is too large: images can be attached up to 5 MB.")
            }
            Self::Unreadable(name, error) => format!("{name} couldn't be read: {error}"),
        }
    }
}

/// The format of an image in one of the formats that can be attached, from
/// its content rather than its name.
pub fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    match imagesize::image_type(bytes).ok()? {
        imagesize::ImageType::Png => Some(ImageFormat::Png),
        imagesize::ImageType::Jpeg => Some(ImageFormat::Jpeg),
        imagesize::ImageType::Gif => Some(ImageFormat::Gif),
        imagesize::ImageType::Webp => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// The media type of an image in one of the formats that can be attached.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    format_of(bytes).map(ImageFormat::mime_type)
}

impl AttachedImage {
    /// `bytes` as an attached image, named `name`, none for one pasted.
    pub fn from_bytes(bytes: Vec<u8>, name: Option<String>) -> Result<Self, Rejected> {
        let shown = name.clone().unwrap_or_else(|| PASTED.to_string());
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Rejected::TooLarge(shown));
        }
        let format = format_of(&bytes).ok_or_else(|| Rejected::NotAnImage(shown.clone()))?;
        let size = imagesize::blob_size(&bytes).map_err(|_| Rejected::NotAnImage(shown))?;
        Ok(Self {
            image: Arc::new(Image::from_bytes(format, bytes)),
            name,
            width: size.width,
            height: size.height,
        })
    }

    /// The image file at `path`, dropped or chosen, as an attached image.
    /// Its size is checked before it is read, so a large file is never read
    /// whole.
    pub fn load(path: &Path) -> Result<Self, Rejected> {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let metadata = fs::metadata(path)
            .map_err(|err| Rejected::Unreadable(name.clone(), err.to_string()))?;
        if !metadata.is_file() {
            return Err(Rejected::NotAnImage(name));
        }
        if metadata.len() > MAX_BYTES {
            return Err(Rejected::TooLarge(name));
        }
        let bytes =
            fs::read(path).map_err(|err| Rejected::Unreadable(name.clone(), err.to_string()))?;
        Self::from_bytes(bytes, Some(name))
    }

    /// An image pasted from the clipboard, as an attached image.
    pub fn pasted(image: &Image) -> Result<Self, Rejected> {
        Self::from_bytes(image.bytes.clone(), None)
    }

    /// What its row calls it: its file's name, or "Pasted image".
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(PASTED)
    }

    /// Its size in pixels, as its row shows it.
    pub fn size_label(&self) -> String {
        format!("{} × {}", self.width, self.height)
    }

    /// Saves it in the project's data, unless the same image is saved there
    /// already, and gives its path from the project directory.
    pub fn save(&self, project_dir: &Path) -> Result<String> {
        save_bytes(&self.image.bytes, self.image.format, project_dir)
    }
}

/// The project's attached images.
pub fn images_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(IMAGES_DIR)
}

/// Saves `bytes`, an image in `format`, in the project's data, named by the
/// second it was saved and a short hash of its content, with its format's
/// extension; the same image saved before is kept rather than saved again.
/// Gives its path from the project directory.
fn save_bytes(bytes: &[u8], format: ImageFormat, project_dir: &Path) -> Result<String> {
    let dir = images_dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let hash = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let suffix = format!("-{}.{}", &hash[..HASH_DIGITS], format.extension());
    let existing = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.ends_with(&suffix));
    let name = match existing {
        Some(name) => name,
        None => {
            let saved_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let name = format!("{saved_at}{suffix}");
            let file = dir.join(&name);
            fs::write(&file, bytes)
                .with_context(|| format!("could not save {}", file.display()))?;
            name
        }
    };
    Ok(format!("{APP_DIR}/{IMAGES_DIR}/{name}"))
}

/// Saves each of `images` as [`AttachedImage::save`] does, in order.
pub fn save_all(images: &[AttachedImage], project_dir: &Path) -> Result<Vec<String>> {
    images.iter().map(|image| image.save(project_dir)).collect()
}

/// The saved images at `paths`, from the project directory, read back to be
/// attached again, as when a queued prompt is edited; any no longer there is
/// left out.
pub fn load_saved(paths: &[String], project_dir: Option<&Path>) -> Vec<AttachedImage> {
    paths
        .iter()
        .filter_map(|path| AttachedImage::load(&resolve(path, project_dir)).ok())
        .collect()
}

/// `path`, from the project directory, as a path to the file.
pub fn resolve(path: &str, project_dir: Option<&Path>) -> PathBuf {
    match project_dir {
        Some(dir) => dir.join(path),
        None => PathBuf::from(path),
    }
}

/// The side of an image's thumbnail beneath a prompt.
pub const PROMPT_THUMBNAIL: Pixels = px(56.);

/// The side of an image's thumbnail beneath a queued prompt.
pub const QUEUED_THUMBNAIL: Pixels = px(32.);

/// The side the larger image shown while hovering a thumbnail fits in.
const HOVER_SIZE: Pixels = px(320.);

/// The images saved at `paths`, from the project directory, as thumbnails
/// `size` square in a row beneath a prompt, each opening the image full size
/// in the platform's image viewer when clicked; none when there are none.
pub fn prompt_thumbnails(
    id: impl Into<ElementId>,
    paths: &[String],
    project_dir: Option<&Path>,
    size: Pixels,
    cx: &App,
) -> Option<AnyElement> {
    if paths.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let (border, radius) = (theme.border, theme.radius);
    let thumbnails = paths.iter().enumerate().map(|(ix, path)| {
        let file = resolve(path, project_dir);
        let name = file
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
        let source = ImageSource::from(file.clone());
        let hover = source.clone();
        let thumbnail = div()
            .id(("prompt-image", ix))
            .flex_none()
            .size(size)
            .overflow_hidden()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .cursor_pointer()
            .child(img(source).size_full().object_fit(ObjectFit::Cover))
            .tooltip(move |window, cx| larger(hover.clone(), Some(name.clone()), window, cx))
            .on_click(move |_, _, cx| cx.open_with_system(&file));
        // Lets UI tests find the thumbnail; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(thumbnail)
    });
    let row = div().id(id).flex().flex_wrap().gap_2().children(thumbnails);
    // Lets UI tests find the row; inert in normal builds.
    Some(gpui_kit::TestSupportExt::test_support(row).into_any_element())
}

/// The larger image shown while a thumbnail is hovered, with its name.
pub fn larger(source: ImageSource, name: Option<String>, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| Larger { source, name }).into()
}

struct Larger {
    source: ImageSource,
    name: Option<String>,
}

impl Render for Larger {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .child(
                img(self.source.clone())
                    .max_w(HOVER_SIZE)
                    .max_h(HOVER_SIZE)
                    .object_fit(ObjectFit::Contain),
            )
            .when_some(self.name.clone(), |this, name| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(name),
                )
            })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{AttachedImage, MAX_BYTES, Rejected};

    /// A fresh directory for test `name`.
    pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("suspense-images-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tiny valid PNG of `width` by `height` pixels (its pixel data isn't
    /// needed to tell its format and size), with `salt` in a text chunk so
    /// images can differ.
    pub(crate) fn png(width: u32, height: u32, salt: &str) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            bytes.extend((data.len() as u32).to_be_bytes());
            bytes.extend(kind);
            bytes.extend(data);
            // The CRC isn't checked here.
            bytes.extend([0; 4]);
        };
        let mut ihdr = Vec::new();
        ihdr.extend(width.to_be_bytes());
        ihdr.extend(height.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"tEXt", salt.as_bytes());
        chunk(b"IEND", &[]);
        bytes
    }

    /// PNG, JPEG, GIF, and WebP images are attached, with their size in
    /// pixels; anything else, or anything over 5 MB, is not, saying why.
    #[test]
    fn only_images_up_to_5_mb_are_attached() {
        let dir = scratch("attach");
        let file = dir.join("shot.png");
        std::fs::write(&file, png(640, 480, "a")).unwrap();
        let image = AttachedImage::load(&file).unwrap();
        assert_eq!(image.label(), "shot.png");
        assert_eq!((image.width, image.height), (640, 480));
        assert_eq!(image.size_label(), "640 × 480");
        assert_eq!(image.image.format, gpui_kit::ImageFormat::Png);

        // By its content, not its name.
        let gif = dir.join("anim.bin");
        std::fs::write(&gif, b"GIF89a\x10\x00\x20\x00\x00\x00\x00;").unwrap();
        let gif = AttachedImage::load(&gif).unwrap();
        assert_eq!((gif.width, gif.height), (16, 32));

        let text = dir.join("notes.png");
        std::fs::write(&text, "not an image").unwrap();
        assert_eq!(
            AttachedImage::load(&text),
            Err(Rejected::NotAnImage("notes.png".into()))
        );
        // A format that can't be attached, though it is an image.
        let bmp = dir.join("old.bmp");
        let mut bytes = b"BM".to_vec();
        bytes.extend([0; 16]);
        bytes.extend(4i32.to_le_bytes());
        bytes.extend(4i32.to_le_bytes());
        bytes.extend([0; 32]);
        std::fs::write(&bmp, bytes).unwrap();
        assert!(matches!(
            AttachedImage::load(&bmp),
            Err(Rejected::NotAnImage(_))
        ));
        assert!(matches!(
            AttachedImage::load(&dir),
            Err(Rejected::NotAnImage(_))
        ));

        let large = dir.join("large.png");
        let mut bytes = png(10, 10, "b");
        bytes.resize(MAX_BYTES as usize + 1, 0);
        std::fs::write(&large, &bytes).unwrap();
        let rejected = AttachedImage::load(&large).unwrap_err();
        assert_eq!(rejected, Rejected::TooLarge("large.png".into()));
        assert!(
            rejected.message().contains("too large"),
            "{}",
            rejected.message()
        );
        // Exactly 5 MB is still attached.
        bytes.truncate(MAX_BYTES as usize);
        std::fs::write(&large, &bytes).unwrap();
        assert!(AttachedImage::load(&large).is_ok());

        // Pasted, it has no name of its own.
        let pasted = AttachedImage::pasted(&gpui_kit::Image::from_bytes(
            gpui_kit::ImageFormat::Png,
            png(3, 4, "c"),
        ))
        .unwrap();
        assert_eq!(pasted.label(), "Pasted image");
        assert!(
            Rejected::NotAnImage("Pasted image".into())
                .message()
                .contains("isn't an image")
        );
    }

    /// Saved images are named by the second they were saved and a short hash
    /// of their content, with their own extension; the same image attached
    /// twice is saved once, and read back as it was.
    #[test]
    fn the_same_image_is_saved_once() {
        let dir = scratch("save");
        let project = dir.as_path();
        let first = AttachedImage::from_bytes(png(2, 2, "one"), None).unwrap();
        let again = AttachedImage::from_bytes(png(2, 2, "one"), Some("copy.png".into())).unwrap();
        let other = AttachedImage::from_bytes(png(2, 2, "two"), None).unwrap();
        let saved = super::save_all(&[first, again, other], project).unwrap();
        assert_eq!(saved[0], saved[1]);
        assert_ne!(saved[0], saved[2]);
        for path in &saved {
            let name = path.strip_prefix(".suspense/images/").unwrap();
            let (second, rest) = name.split_once('-').unwrap();
            assert!(second.parse::<u64>().is_ok(), "{name}");
            assert_eq!(rest.len(), super::HASH_DIGITS + ".png".len(), "{name}");
            assert!(rest.ends_with(".png"), "{name}");
        }
        assert_eq!(
            std::fs::read_dir(super::images_dir(project))
                .unwrap()
                .count(),
            2
        );
        let read = super::load_saved(&saved, Some(project));
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].image.bytes, png(2, 2, "one"));
        // One no longer there is left out.
        assert_eq!(
            super::load_saved(&["gone.png".into()], Some(project)).len(),
            0
        );
    }
}
