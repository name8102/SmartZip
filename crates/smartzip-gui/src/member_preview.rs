//! Bounded, passive member viewing; never writes or executes archive contents.
use gpui::base::Disableable;
use gpui::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    ActiveTheme, Sizable,
};
use gpui::{
    div, img, prelude::*, px, AppContext, Context, Entity, Image, ImageFormat, IntoElement,
    ObjectFit, Render, Window,
};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct MemberSource {
    pub archive: PathBuf,
    pub member: PathBuf,
    pub backend: String,
    pub config: smartzip_config::BackendConfig,
    pub encoding: smartzip_core::EncodingMode,
    pub unavailable: Option<String>,
    pub allow_password: bool,
    pub max_bytes: usize,
}

enum Content {
    Text(String),
    Image(Arc<Image>),
}
pub struct MemberPreview {
    source: Option<MemberSource>,
    content: Option<Content>,
    message: String,
    password: Entity<InputState>,
    cancel: CancellationToken,
    generation: u64,
    busy: bool,
    tx: mpsc::Sender<(u64, Result<Content, String>)>,
    rx: mpsc::Receiver<(u64, Result<Content, String>)>,
}
impl Drop for MemberPreview {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl MemberPreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::channel();
        cx.spawn(async move |entity, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if entity
                .update(cx, |this, cx| {
                    while let Ok((generation, result)) = this.rx.try_recv() {
                        if generation != this.generation {
                            continue;
                        }
                        this.busy = false;
                        match result {
                            Ok(content) => {
                                this.content = Some(content);
                                this.message.clear();
                            }
                            Err(error) => this.message = error,
                        }
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        Self {
            source: None,
            content: None,
            message: "选择归档内文件查看内容".into(),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("加密文件的密码")
            }),
            cancel: CancellationToken::new(),
            generation: 0,
            busy: false,
            tx,
            rx,
        }
    }
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.generation += 1;
        self.source = None;
        self.content = None;
        self.busy = false;
        self.message = "选择归档内文件查看内容".into();
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }
    pub fn open(&mut self, source: MemberSource, window: &mut Window, cx: &mut Context<Self>) {
        self.clear(window, cx);
        self.source = Some(source);
        self.read(None, cx);
    }
    fn read(&mut self, password: Option<String>, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.generation += 1;
        self.content = None;
        let Some(source) = self.source.clone() else {
            return;
        };
        if password.is_some() && !source.allow_password {
            self.message = "当前配置已关闭密码功能".into();
            cx.notify();
            return;
        }
        if let Some(reason) = &source.unavailable {
            self.message = reason.clone();
            cx.notify();
            return;
        }
        self.cancel = CancellationToken::new();
        let token = self.cancel.clone();
        let generation = self.generation;
        let tx = self.tx.clone();
        self.busy = true;
        self.message = "正在读取所选文件…".into();
        std::thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                let backend = smartzip_archive::BackendRouter::from_config(&source.config)
                    .map_err(|e| e.to_string())?;
                let bytes = runtime
                    .block_on(backend.read_member(
                        &source.backend,
                        smartzip_archive::MemberReadRequest {
                            archive: source.archive,
                            member: source.member,
                            password,
                            encoding: source.encoding,
                            max_bytes: source.max_bytes,
                        },
                        token.clone(),
                    ))
                    .map_err(|e| e.to_string())?;
                if token.is_cancelled() {
                    return Err("已停止预览".into());
                }
                decode(&bytes)
            })();
            let _ = tx.send((generation, result));
        });
        cx.notify();
    }
}
fn decode(bytes: &[u8]) -> Result<Content, String> {
    if let Ok(format) = image::guess_format(bytes) {
        if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
            return Err("此图片格式暂不支持内置预览，可解压后查看".into());
        }
        let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
            .into_dimensions()
            .map_err(|_| "图片头损坏")?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_000_000 {
            return Err("图片超过 1600 万像素预览上限".into());
        }
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(96 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .map_err(|_| "图片损坏或解码资源超限")?
            .thumbnail(2048, 2048);
        let mut png = Cursor::new(Vec::new());
        decoded
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|_| "图片转换失败")?;
        return Ok(Content::Image(Arc::new(Image::from_bytes(
            ImageFormat::Png,
            png.into_inner(),
        ))));
    }
    if bytes.len() > 1024 * 1024 {
        return Err("文本超过 1 MiB 预览上限，可解压后查看".into());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| "暂不支持此文件类型或文本编码，可解压后查看")?;
    if text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err("二进制文件不支持文本预览，可解压后查看".into());
    }
    Ok(Content::Text(text.to_owned()))
}
impl Render for MemberPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .source
            .as_ref()
            .map(|source| source.member.display().to_string())
            .unwrap_or_else(|| "文件内容".into());
        div()
            .id("member-content")
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_base().child(title))
            .when(!self.message.is_empty(), |el| {
                el.child(div().text_sm().child(self.message.clone()))
            })
            .when_some(self.content.as_ref(), |el, content| match content {
                Content::Text(text) => el.child(
                    div()
                        .id("member-text")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(text.clone()),
                ),
                Content::Image(image) => el.child(
                    img(image.clone())
                        .w_full()
                        .h(px(360.))
                        .object_fit(ObjectFit::Contain),
                ),
            })
            .when(
                self.source
                    .as_ref()
                    .is_some_and(|source| source.unavailable.is_none()),
                |el| {
                    el.child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().w(px(230.)).child(Input::new(&self.password).small()))
                            .child(
                                Button::new("member-retry")
                                    .small()
                                    .ghost()
                                    .label("使用密码重试")
                                    .disabled(
                                        self.busy
                                            || !self
                                                .source
                                                .as_ref()
                                                .is_some_and(|source| source.allow_password),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let password = this.password.read(cx).value().to_string();
                                        this.password.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        this.read(
                                            if password.is_empty() {
                                                None
                                            } else {
                                                Some(password)
                                            },
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("member-stop")
                                    .small()
                                    .ghost()
                                    .label("停止")
                                    .disabled(!self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel.cancel();
                                        this.generation += 1;
                                        this.busy = false;
                                        this.message = "已停止预览".into();
                                        cx.notify();
                                    })),
                            ),
                    )
                },
            )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passive_text_preserves_markup_and_rejects_binary_and_large_inputs() {
        assert!(matches!(
            decode(b"<script>hello</script>"),
            Ok(Content::Text(_))
        ));
        assert!(decode(b"MZ\0binary").is_err());
        assert!(decode(&vec![b'a'; 1024 * 1024 + 1]).is_err());
    }
    #[test]
    fn small_png_decodes_to_bounded_still_image() {
        let mut data = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        assert!(matches!(decode(data.get_ref()), Ok(Content::Image(_))));
    }
}
