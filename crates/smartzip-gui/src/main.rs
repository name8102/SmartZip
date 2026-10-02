mod appearance;
mod archive_browser;
mod assets;
mod config_form;
mod config_lists;
mod instance;
mod integration_ui;
mod library;
mod member_preview;
mod model;
mod new_open_request;
mod path_reports;
mod preferences;
mod queue_store;
mod runtime;
mod settings_fields;
mod ui;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--finder-quick-extract")
    {
        if let Err(error) = new_open_request::forward_finder_quick_extract(&args[1..]) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let (open_sender, open_receiver) = std::sync::mpsc::channel();
    let mut initial = new_open_request::parse_args(args);
    for path in &mut initial.paths {
        if let Ok(absolute) = std::path::absolute(&*path) {
            *path = absolute;
        }
    }
    #[cfg(unix)]
    let _instance = match preferences::Preferences::path().and_then(|path| {
        instance::claim(
            &std::path::absolute(path).map_err(|e| e.to_string())?,
            &initial,
            open_sender.clone(),
        )
        .map_err(|e| e.to_string())
    }) {
        Ok(Some(owner)) => owner,
        Ok(None) => return,
        Err(error) => {
            eprintln!("无法连接桌面实例：{error}");
            std::process::exit(1);
        }
    };
    let _ = open_sender.send(initial);
    let url_sender = open_sender.clone();

    let app = gpui::application().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        for request in new_open_request::requests_from_urls(urls) {
            let _ = url_sender.send(request);
        }
    });
    app.run(move |cx| {
        gpui::init(cx);
        ui::start(cx, open_receiver);
    });
}
