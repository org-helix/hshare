mod app;
mod capture;
mod hid;
mod i18n;
mod net;
mod theme;
mod titlebar;

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--version") {
        println!("hshare {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: titlebar::viewport(eframe::egui::ViewportBuilder::default())
            .with_title("hShare")
            .with_app_id("hshare")
            .with_inner_size([1040.0, 640.0])
            .with_min_inner_size([480.0, 420.0])
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/hshare.png"))
                    .expect("bundled icon is a valid PNG"),
            )),
        ..Default::default()
    };
    eframe::run_native("hshare", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
