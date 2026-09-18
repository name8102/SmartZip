mod archive_browser;
mod assets;
mod config_form;
mod integration_ui;
mod library;
mod member_preview;
mod model;
mod new_open_request;
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
    let _ = open_sender.send(new_open_request::parse_args(args));
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
