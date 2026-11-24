use crate::i18n::{self, Lang, tr};
use crate::net::{self, Event, Net};
use crate::theme::{self, pal};
use crate::{tf, titlebar};
use eframe::egui::{
    self, CentralPanel, Color32, CornerRadius, Frame, Key, Margin, Panel, RichText,
    TextureHandle, TextureOptions, Ui, ViewportCommand, vec2,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};

const PREFS_KEY: &str = "hshare-prefs";

/// Settings kept between sessions.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    lang: Lang,
    dark: bool,
    target: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { lang: i18n::system_lang(), dark: true, target: String::new() }
    }
}

/// Our screen going out.
enum Cast {
    Idle,
    Connecting,
    Live(String),
}

/// Another screen shown here.
struct View {
    /// None while waiting for the other side to accept.
    peer: Option<String>,
    texture: Option<TextureHandle>,
}

struct Status {
    text: String,
    error: bool,
    /// A received file, offered to be shown in its folder.
    file: Option<PathBuf>,
}

pub struct App {
    net: Arc<Net>,
    events: Receiver<Event>,
    ip: String,
    target: String,
    dark: bool,
    cast: Cast,
    view: Option<View>,
    requests: Vec<(String, Sender<bool>)>,
    status: Option<Status>,
    usb: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs: Prefs = cc.storage.and_then(|s| eframe::get_value(s, PREFS_KEY)).unwrap_or_default();
        i18n::set_lang(prefs.lang);
        theme::install(&cc.egui_ctx, prefs.dark);
        let ctx = cc.egui_ctx.clone();
        let (net, events) = net::start(move || ctx.request_repaint());
        crate::hid::watch(net.clone());
        Self {
            ip: net::local_ip().map(|ip| ip.to_string()).unwrap_or_default(),
            net,
            events,
            target: prefs.target,
            dark: prefs.dark,
            cast: Cast::Idle,
            view: None,
            requests: Vec::new(),
            status: None,
            usb: false,
        }
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status { text: text.into(), error, file: None });
    }

    fn handle(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::ViewStarted { peer } => {
                self.view = Some(View { peer: Some(peer), texture: None });
            }
            Event::Frame(img) => {
                let image = egui::ColorImage::from_rgb([img.width, img.height], &img.rgb);
                if let Some(view) = &mut self.view {
                    match &mut view.texture {
                        Some(t) => t.set(image, TextureOptions::LINEAR),
                        None => view.texture = Some(ctx.load_texture("screen", image, TextureOptions::LINEAR)),
                    }
                }
            }
            Event::ViewEnded { peer, error } => {
                self.view = None;
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                match error {
                    Some(e) => self.say(e, true),
                    None if !peer.is_empty() => self.say(tf!("{peer} a arrêté le partage.", "{peer} stopped sharing."), false),
                    None => {}
                }
            }
            Event::ReverseRequest { peer, reply } => {
                self.requests.push((peer, reply));
                ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
            Event::CastStarted { peer } => self.cast = Cast::Live(peer),
            Event::CastEnded { error } => {
                self.cast = Cast::Idle;
                match error {
                    Some(e) => self.say(e, true),
                    None => self.say(tr("Partage arrêté.", "Sharing stopped."), false),
                }
            }
            Event::FileReceived { path, from } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.status = Some(Status {
                    text: tf!("Fichier reçu de {from} : {name}", "File received from {from}: {name}"),
                    error: false,
                    file: Some(path),
                });
            }
            Event::FileSent { name, to } => self.say(tf!("{name} envoyé à {to}.", "{name} sent to {to}."), false),
            Event::Error(e) => self.say(e, true),
            Event::UsbButton { connected } => self.usb = connected,
            Event::UsbPressed => self.toggle_cast(),
        }
    }

    fn toggle_cast(&mut self) {
        if matches!(self.cast, Cast::Idle) {
            self.start_cast();
        } else {
            self.net.casting.store(false, Ordering::SeqCst);
        }
    }

    /// Checks the code field; reports a problem in the status line.
    fn valid_target(&mut self) -> Option<String> {
        match net::parse_target(&self.target) {
            Ok(_) => Some(self.target.clone()),
            Err(e) => {
                self.say(e, true);
                None
            }
        }
    }

    fn start_cast(&mut self) {
        if let Some(t) = self.valid_target() {
            self.status = None;
            self.cast = Cast::Connecting;
            net::cast(self.net.clone(), t);
        }
    }

    fn start_reverse(&mut self) {
        if let Some(t) = self.valid_target() {
            self.status = None;
            self.view = Some(View { peer: None, texture: None });
            net::reverse(self.net.clone(), t);
        }
    }

    fn pick_file(&mut self) {
        if let Some(t) = self.valid_target()
            && let Some(path) = rfd::FileDialog::new().pick_file()
        {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.say(tf!("Envoi de {name}…", "Sending {name}…"), false);
            net::send_file(self.net.clone(), t, path);
        }
    }

    fn stop_view(&mut self, ctx: &egui::Context) {
        self.net.viewing.store(false, Ordering::SeqCst);
        self.view = None;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
    }

    pub fn menus(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.menu_button(tr("Affichage", "View"), |ui| {
            if ui.checkbox(&mut self.dark, tr("Mode sombre", "Dark mode")).changed() {
                theme::apply(&ctx, self.dark);
            }
            ui.separator();
            ui.menu_button(tr("Langue", "Language"), |ui| {
                for lang in Lang::ALL {
                    if ui.radio(i18n::lang() == lang, lang.native_name()).clicked() {
                        i18n::set_lang(lang);
                        ui.close();
                    }
                }
            });
        });
        ui.menu_button(tr("Aide", "Help"), |ui| {
            ui.set_max_width(420.0);
            ui.label(theme::muted(tr(
                "hShare — partage d'écran sur le réseau local",
                "hShare — screen sharing on the local network",
            )));
            ui.label(theme::muted(format!("Version {}", env!("CARGO_PKG_VERSION"))));
            ui.separator();
            ui.label(tr(
                "Chaque appareil affiche un code. Saisissez le code de l'autre appareil pour lui \
                 envoyer votre écran, voir le sien ou lui envoyer un fichier.",
                "Each device shows a code. Type the other device's code to send it your screen, \
                 see its screen, or send it a file.",
            ));
            ui.label(tr(
                "Si le code n'est pas trouvé (réseaux différents), saisissez son adresse IP puis le \
                 code, par exemple 192.168.1.20 482913.",
                "If the code isn't found (different networks), type its IP address then the code, \
                 for example 192.168.1.20 482913.",
            ));
            ui.label(tr(
                "Écran reçu : double-clic ou F11 pour le plein écran, Échap pour en sortir.",
                "Incoming screen: double-click or F11 for full screen, Esc to leave it.",
            ));
            ui.label(theme::muted(tf!(
                "Ports utilisés : TCP {}, UDP {}. Le trafic n'est pas chiffré : à utiliser sur un réseau de confiance.",
                "Ports used: TCP {}, UDP {}. Traffic is not encrypted: use it on a trusted network.",
                net::PORT,
                net::DISCOVERY_PORT
            )));
        });
    }

    fn home(&mut self, ui: &mut Ui) {
        let c = pal();
        let width = ui.available_width().min(920.0);
        let side = (ui.available_width() - width) / 2.0;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(40.0);
            ui.horizontal(|ui| {
                ui.add_space(side.max(16.0));
                ui.vertical(|ui| {
                    ui.set_max_width(width - 32.0);
                    if width > 760.0 {
                        ui.columns(2, |cols| {
                            card(&mut cols[0], |ui| self.receive_card(ui));
                            card(&mut cols[1], |ui| self.share_card(ui));
                        });
                    } else {
                        card(ui, |ui| self.receive_card(ui));
                        card(ui, |ui| self.share_card(ui));
                    }
                    if let Some(s) = &self.status {
                        ui.add_space(8.0);
                        let color = if s.error { c.error } else { c.success };
                        let mut open = None;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&s.text).color(color));
                            if let Some(f) = &s.file
                                && theme::ghost_button(ui, tr("Ouvrir le dossier", "Open folder")).clicked()
                            {
                                open = f.parent().map(PathBuf::from);
                            }
                        });
                        if let Some(dir) = open {
                            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                        }
                    }
                    if self.usb {
                        ui.add_space(8.0);
                        ui.label(theme::muted(tr(
                            "Bouton USB connecté : appuyez dessus pour démarrer ou arrêter le partage vers le code saisi.",
                            "USB button connected: press it to start or stop sharing to the code entered.",
                        )));
                    }
                });
            });
        });
    }

    fn receive_card(&mut self, ui: &mut Ui) {
        let c = pal();
        ui.label(theme::title(tr("Recevoir", "Receive")));
        ui.add_space(4.0);
        ui.label(theme::eyebrow(tr("Code de cet appareil", "This device's code")));
        let code = &self.net.code;
        ui.label(
            RichText::new(format!("{} {}", &code[..3], &code[3..]))
                .family(theme::display())
                .size(56.0)
                .color(c.text)
                .extra_letter_spacing(2.0),
        );
        ui.label(theme::muted(if self.ip.is_empty() { self.net.name.clone() } else { format!("{} · {}", self.net.name, self.ip) }));
        ui.add_space(8.0);
        ui.label(tr(
            "Sur l'autre ordinateur, ouvrez hShare et saisissez ce code pour afficher son écran ici.",
            "On the other computer, open hShare and type this code to show its screen here.",
        ));
    }

    fn share_card(&mut self, ui: &mut Ui) {
        let c = pal();
        ui.label(theme::title(tr("Partager", "Share")));
        ui.add_space(4.0);
        ui.label(theme::eyebrow(tr("Code de l'autre appareil", "The other device's code")));
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.target)
                .hint_text("482 913")
                .font(egui::FontId::new(20.0, theme::medium()))
                .margin(Margin::symmetric(12, 8))
                .desired_width(f32::INFINITY),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(8.0);
        match &self.cast {
            Cast::Idle => {
                if theme::primary_button(ui, tr("Partager mon écran", "Share my screen")).clicked() || enter {
                    self.start_cast();
                }
            }
            Cast::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Connexion… choisissez l'écran à partager.", "Connecting… choose the screen to share."));
                });
                if theme::ghost_button(ui, tr("Annuler", "Cancel")).clicked() {
                    self.net.casting.store(false, Ordering::SeqCst);
                }
            }
            Cast::Live(peer) => {
                let peer = peer.clone();
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, c.accent);
                    ui.label(RichText::new(tf!("Votre écran est affiché sur {peer}", "Your screen is shown on {peer}")).color(c.text));
                });
                if theme::primary_button(ui, tr("Arrêter le partage", "Stop sharing")).clicked() {
                    self.net.casting.store(false, Ordering::SeqCst);
                }
            }
        }
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(tr("Voir son écran", "See their screen")).min_size(vec2(0.0, 32.0))).clicked() {
                self.start_reverse();
            }
            if ui.add(egui::Button::new(tr("Envoyer un fichier…", "Send a file…")).min_size(vec2(0.0, 32.0))).clicked() {
                self.pick_file();
            }
        });
    }

    fn viewer(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let c = pal();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
        let Some(view) = &self.view else { return };
        let peer = view.peer.clone();
        match (&view.texture, &peer) {
            (Some(tex), _) => {
                let size = tex.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let img_rect = egui::Rect::from_center_size(rect.center(), size * scale);
                ui.painter().image(tex.id(), img_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            (None, None) => {
                ui.put(rect, egui::Label::new(RichText::new(tr("En attente de l'accord de l'autre appareil…", "Waiting for the other device to accept…")).color(Color32::WHITE)));
            }
            (None, Some(_)) => {
                ui.put(rect, egui::Spinner::new().size(32.0).color(Color32::WHITE));
            }
        }
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let resp = ui.interact(rect, egui::Id::unique("hshare-view"), egui::Sense::click());
        if resp.double_clicked() || ctx.input(|i| i.key_pressed(Key::F11)) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!full));
        }
        if full && ctx.input(|i| i.key_pressed(Key::Escape)) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        }
        // Controls float at the top, hidden in full screen until the pointer comes near.
        let near_top = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y < rect.top() + 80.0));
        if full && !near_top {
            return;
        }
        let mut stop = false;
        egui::Area::new(egui::Id::unique("hshare-view-bar"))
            .fixed_pos(rect.left_top() + vec2(16.0, 16.0))
            .show(&ctx, |ui| {
                Frame::new().fill(c.panel).corner_radius(CornerRadius::same(12)).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let who = peer.as_deref().unwrap_or("…");
                        ui.label(RichText::new(tf!("Écran de {who}", "{who}'s screen")).family(theme::medium()).color(c.text));
                        let label = if full { tr("Quitter le plein écran", "Exit full screen") } else { tr("Plein écran", "Full screen") };
                        if theme::ghost_button(ui, label).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!full));
                        }
                        if theme::primary_button(ui, tr("Arrêter", "Stop")).clicked() {
                            stop = true;
                        }
                    });
                });
            });
        if stop {
            self.stop_view(&ctx);
        }
    }

    fn requests(&mut self, ctx: &egui::Context) {
        let Some((peer, _)) = self.requests.first() else { return };
        let peer = peer.clone();
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("hshare-reverse")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(theme::title(tf!("{peer} veut voir votre écran", "{peer} wants to see your screen")));
            ui.add_space(4.0);
            ui.label(theme::muted(tr(
                "Si vous acceptez, choisissez ensuite l'écran ou la fenêtre à montrer.",
                "If you accept, you then choose the screen or window to show.",
            )));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, tr("Autoriser", "Allow")).clicked() {
                    answer = Some(true);
                }
                if ui.add(egui::Button::new(tr("Refuser", "Decline")).min_size(vec2(0.0, 36.0))).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    answer = Some(false);
                }
            });
        });
        if let Some(yes) = answer {
            let (_, reply) = self.requests.remove(0);
            let _ = reply.send(yes);
            if yes {
                self.cast = Cast::Connecting;
            }
        }
    }
}

/// A content card: translucent block, no border.
fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(pal().panel)
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(280.0);
            ui.vertical(add);
        });
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let prefs = Prefs { lang: i18n::lang(), dark: self.dark, target: self.target.clone() };
        eframe::set_value(storage, PREFS_KEY, &prefs);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(e) = self.events.try_recv() {
            self.handle(&ctx, e);
        }
        let window = ui.max_rect();
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !full {
            Panel::top("titlebar")
                .exact_size(titlebar::HEIGHT)
                .frame(Frame::new().fill(pal().panel))
                .show(ui, |ui| titlebar::show(self, ui));
        }
        if self.view.is_some() {
            CentralPanel::no_frame().show(ui, |ui| self.viewer(ui));
        } else {
            CentralPanel::no_frame()
                .frame(Frame::new().fill(pal().canvas))
                .show(ui, |ui| self.home(ui));
        }
        titlebar::resize_edges(ui, window);
        self.requests(&ctx);
    }
}
