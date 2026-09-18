use gpui::base::Disableable;
use gpui::component::{
    button::{Button, ButtonVariants},
    ActiveTheme, Sizable,
};
use gpui::{div, prelude::*, Context, IntoElement, Render, Window};
use smartzip_platform::system_integration::{self, IntegrationStatus};
use std::{sync::mpsc, time::Duration};

enum Reply {
    Status(Result<IntegrationStatus, String>),
    Operation(Result<String, String>),
}
pub struct SystemIntegration {
    status: Option<IntegrationStatus>,
    busy: bool,
    message: String,
    tx: mpsc::Sender<Reply>,
    rx: mpsc::Receiver<Reply>,
}
impl SystemIntegration {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::channel();
        cx.spawn(async move |entity, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if entity
                .update(cx, |this, cx| {
                    while let Ok(reply) = this.rx.try_recv() {
                        match reply {
                            Reply::Status(result) => {
                                this.busy = false;
                                match result {
                                    Ok(status) => this.status = Some(status),
                                    Err(error) => this.message = error,
                                };
                            }
                            Reply::Operation(result) => {
                                this.message = result.unwrap_or_else(|error| error);
                                this.refresh(cx);
                            }
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
            status: None,
            busy: false,
            message: String::new(),
            tx,
            rx,
        }
    }
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Reply::Status(
                system_integration::status().map_err(|e| e.to_string()),
            ));
        });
        cx.notify();
    }
    fn operation(&mut self, action: fn() -> Result<String, String>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Reply::Operation(action()));
        });
        cx.notify();
    }
}
impl Render for SystemIntegration {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bundle = self
            .status
            .as_ref()
            .and_then(|status| status.bundle.clone());
        let installed = self
            .status
            .as_ref()
            .is_some_and(|status| status.finder_installed);
        let formats = self
            .status
            .as_ref()
            .map(|status| status.associations.clone())
            .unwrap_or_default();
        div().flex().flex_col().gap_4()
            .child(div().text_base().child("系统集成"))
            .child(div().text_sm().child("普通打开压缩包进入预览；右键快速解压进入快速窗口。"))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(bundle.as_ref().map(|path|path.display().to_string()).unwrap_or_else(||"请从固定位置的 SmartZip.app 启动后配置。开发用裸可执行文件不能注册为默认程序。".into())))
            .child(div().flex().gap_2()
                .child(Button::new("integration-refresh").small().ghost().label("刷新状态").disabled(self.busy).on_click(cx.listener(|this,_,_,cx|this.refresh(cx))))
                .child(Button::new("integration-register").small().label("注册打开方式").disabled(self.busy||bundle.is_none()).on_click(cx.listener(|this,_,_,cx|this.operation(||system_integration::register_application().map(|_|"已注册为可选打开方式".into()).map_err(|e|e.to_string()),cx))))
                .when_some(bundle.clone(),|el,path|el.child(Button::new("integration-reveal").small().ghost().label("显示应用位置").on_click(move|_,_,cx|cx.reveal_path(&path)))))
            .child(div().text_sm().child("默认打开方式"))
            .children(formats.into_iter().map(|association| {
                let extension=association.format.extension.clone();
                div().flex().items_center().gap_3().py_2().border_b_1().border_color(cx.theme().border)
                    .child(div().w(gpui::px(110.)).child(format!("{} · .{}",association.format.label,extension)))
                    .child(div().flex_1().text_sm().text_color(cx.theme().muted_foreground).child(
                        if association.is_default {"SmartZip（当前默认）".into()} else {association.application.and_then(|p|p.file_name().map(|s|s.to_string_lossy().into_owned())).unwrap_or_else(||"未指定默认程序".into())}))
                    .child(Button::new(gpui::SharedString::from(format!("default-{extension}"))).small().label("设为默认").disabled(self.busy||bundle.is_none()||association.is_default)
                        .on_click(cx.listener(move|this,_,_,cx| {
                            if this.busy{return;} this.busy=true;this.message="等待系统完成设置；如弹出确认，请在系统提示中选择".into();
                            let tx=this.tx.clone();let label=extension.clone();
                            if let Err(error)=system_integration::set_default(&extension,Box::new(move|result|{let _=tx.send(Reply::Operation(result.map(|_|format!(".{label} 的默认程序已更新"))));})) {this.busy=false;this.message=error.to_string();}
                            cx.notify();
                        })))
            }))
            .child(div().text_sm().child("Finder 右键快速解压"))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(if installed {"已安装。在 Finder 选择文件，右键 → 快速操作 → SmartZip 快速解压。"} else {"安装后，在 Finder 的右键“快速操作”中选择 SmartZip 快速解压，支持多选文件。"}))
            .child(div().flex().gap_2()
                .child(Button::new("finder-install").small().label("安装右键菜单").disabled(self.busy||bundle.is_none()||installed).on_click(cx.listener(|this,_,_,cx|this.operation(||system_integration::install_finder_service().map(|_|"右键快速解压已安装；若菜单未出现，请在系统设置的扩展中启用该快速操作".into()).map_err(|e|e.to_string()),cx))))
                .child(Button::new("finder-remove").small().ghost().label("移除右键菜单").disabled(self.busy||!installed).on_click(cx.listener(|this,_,_,cx|this.operation(||system_integration::remove_finder_service().map(|_|"已移除 SmartZip 右键菜单".into()).map_err(|e|e.to_string()),cx)))))
            .when(!self.message.is_empty(),|el|el.child(div().text_sm().child(self.message.clone())))
    }
}
