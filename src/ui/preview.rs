use std::sync::Arc;

use gpui::{
    div, img, prelude::*, App, Context, FocusHandle, Focusable, Image, ImageFormat, SharedString,
    Window,
};
use gpui_component::text::TextView;

use crate::host::Host;
use crate::theme::Theme;
use crate::workspace::item::ItemId;

/// An image is decoded into a texture whole, so a mistaken double-click on a
/// video shouldn't try. Markdown goes through `read_text`, which has the
/// editor's own 5 MB limit.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// What the tab is showing. The file is fetched once, when the tab opens.
enum Content {
    Loading,
    Markdown(SharedString),
    Image(Arc<Image>),
    Failed(SharedString),
}

/// A read-only look at a remote file: Markdown rendered, or an image drawn.
///
/// A tab of its own rather than a mode of the editor, so a preview and the
/// source can sit side by side in a split. Nothing here writes: the editor
/// remains the only thing that saves.
pub struct PreviewView {
    pub item_id: ItemId,
    pub path: String,
    pub focus_handle: FocusHandle,
    name: SharedString,
    content: Content,
}

impl PreviewView {
    pub fn new(host: Host, path: String, cx: &mut Context<Self>) -> Self {
        let name: SharedString = file_name(&path).to_string().into();
        let mut this = Self {
            item_id: ItemId::next(),
            path,
            focus_handle: cx.focus_handle(),
            name,
            content: Content::Loading,
        };
        this.load(host, cx);
        this
    }

    pub fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn load(&mut self, host: Host, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let format = image_format(&self.name);
        cx.spawn(async move |this, cx| {
            let content = match format {
                Some(format) => match host.files.read_bytes(path, MAX_IMAGE_BYTES).await {
                    // gpui decodes from the bytes and the format we name, so
                    // nothing here depends on the file's extension being honest
                    // beyond picking the decoder.
                    Ok(bytes) => Content::Image(Arc::new(Image::from_bytes(format, bytes))),
                    Err(error) => Content::Failed(error.into()),
                },
                None => match host.files.read_text(path).await {
                    Ok(text) => Content::Markdown(text.into()),
                    Err(error) => Content::Failed(error.into()),
                },
            };
            let _ = this.update(cx, |this, cx| {
                this.content = content;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Focusable for PreviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (fg, fg_subtle, danger) = {
            let t = cx.global::<Theme>();
            (t.fg, t.fg_subtle, t.danger)
        };

        let body = match &self.content {
            Content::Loading => div()
                .p_2()
                .text_color(fg_subtle)
                .child("loading…")
                .into_any_element(),
            Content::Failed(error) => div()
                .p_2()
                .text_color(danger)
                .child(error.clone())
                .into_any_element(),
            Content::Markdown(source) => div()
                .size_full()
                .p_4()
                .text_color(fg)
                .child(
                    // Rendered by gpui-component, which highlights fenced code
                    // with the same tree-sitter set the editor uses.
                    //
                    // `scrollable` is the view's *own* mode -- it swaps the
                    // block list for a virtualised one with a scrollbar. An
                    // `overflow_y_scroll` container around it scrolls nothing,
                    // because the view sizes itself to the space it is given:
                    // the content is clipped, not overflowing, so a long
                    // document simply stopped at the bottom of the pane.
                    TextView::markdown(
                        ("markdown", self.item_id.0 as usize),
                        source.clone(),
                        window,
                        cx,
                    )
                    .selectable(true)
                    .scrollable(true),
                )
                .into_any_element(),
            Content::Image(image) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_2()
                // gpui's images contain-fit by default, so this scales to the
                // pane and keeps the aspect ratio.
                .child(img(image.clone()).size_full())
                .into_any_element(),
        };

        div()
            .id("preview")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(body)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn extension(name: &str) -> Option<String> {
    Some(name.rsplit_once('.')?.1.to_ascii_lowercase())
}

/// The image formats gpui can decode, by extension. `None` means "not an
/// image", which is how the preview decides between a picture and Markdown.
pub fn image_format(name: &str) -> Option<ImageFormat> {
    Some(match extension(file_name(name))?.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

pub fn is_markdown(name: &str) -> bool {
    matches!(
        extension(file_name(name)).as_deref(),
        Some("md" | "markdown" | "mdx")
    )
}

/// Whether the explorer should offer "Preview" for this file.
pub fn can_preview(name: &str) -> bool {
    image_format(name).is_some() || is_markdown(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_what_it_can_show() {
        assert!(matches!(image_format("shot.PNG"), Some(ImageFormat::Png)));
        assert!(matches!(image_format("a.jpeg"), Some(ImageFormat::Jpeg)));
        assert!(image_format("notes.md").is_none());
        assert!(is_markdown("/home/rare/README.md"));
        assert!(!is_markdown("main.rs"));
        assert!(can_preview("diagram.svg") && can_preview("notes.markdown"));
        assert!(!can_preview("server.log"));
    }

    /// A dot in a *directory* name mustn't make the file look like an image --
    /// the extension is taken from the last path segment, not the whole path.
    #[test]
    fn extensions_come_from_the_file_not_the_path() {
        assert!(image_format("/var/www/site.png/index").is_none());
        assert!(image_format("/var/www/site.png/logo.png").is_some());
        assert!(image_format("/no/extension/here").is_none());
    }
}
