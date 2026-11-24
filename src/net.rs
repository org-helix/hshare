//! hShare protocol, between two hShare instances on the same network.
//!
//! Every instance listens on TCP [`PORT`] and answers discovery on UDP [`DISCOVERY_PORT`].
//! It shows a random 6-digit code: whoever wants to reach it types that code, which
//! finds it by UDP broadcast and proves the person can see its screen.
//!
//! TCP messages are `[kind: u8][length: u32 BE][payload]`. A session starts with
//! `Hello` (JSON) and gets `Accept` or `Reject`, then:
//! - cast: the caller streams `Frame`s (JPEG) that the callee displays;
//! - reverse: the callee asks its user, then streams its own screen back;
//! - file: the caller sends a `File` header (JSON name and size) then the raw bytes.
//!
//! Traffic is not encrypted: meant for a trusted local network.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
const MAX_MESSAGE: usize = 64 << 20;

const HELLO: u8 = 1;
const ACCEPT: u8 = 2;
const REJECT: u8 = 3;
const FRAME: u8 = 4;
const FILE: u8 = 5;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Cast,
    Reverse,
    File,
}

#[derive(Serialize, Deserialize, Debug)]
struct Hello {
    name: String,
    code: String,
    purpose: Purpose,
}

#[derive(Serialize, Deserialize)]
struct FileHeader {
    name: String,
    size: u64,
}

fn write_msg(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = [kind, 0, 0, 0, 0];
    head[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

fn read_msg(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes(head[1..].try_into().unwrap()) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok((head[0], buf))
}

fn err(e: impl ToString) -> io::Error {
    io::Error::other(e.to_string())
}

/// A decoded frame ready for display.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// What the network tells the UI.
pub enum Event {
    /// Someone started casting to us, or the screen we asked for started arriving.
    ViewStarted { peer: String },
    Frame(Image),
    ViewEnded { peer: String, error: Option<String> },
    /// `peer` wants to see our screen; answer on `reply`.
    ReverseRequest { peer: String, reply: Sender<bool> },
    /// Our outgoing cast ended (stopped, refused, or failed).
    CastEnded { error: Option<String> },
    CastStarted { peer: String },
    FileReceived { path: PathBuf, from: String },
    FileSent { name: String, to: String },
    Error(String),
    UsbButton { connected: bool },
    UsbPressed,
}

/// Shared state between the UI and the network threads.
pub struct Net {
    pub name: String,
    pub code: String,
    pub port: u16,
    events: Mutex<Sender<Event>>,
    wake: Box<dyn Fn() + Send + Sync>,
    /// Set while we display an incoming cast or reverse view; cleared to stop it.
    pub viewing: AtomicBool,
    /// Set while our screen goes out (cast or accepted reverse); cleared to stop it.
    pub casting: AtomicBool,
    /// Where received files go.
    inbox: PathBuf,
}

impl Net {
    pub fn send(&self, e: Event) {
        let _ = self.events.lock().unwrap().send(e);
        (self.wake)();
    }
}

/// Starts listening. Events reach the UI through the returned receiver; `wake` repaints it.
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> (Arc<Net>, std::sync::mpsc::Receiver<Event>) {
    // HSHARE_PORT runs a second copy on one machine (for testing).
    let port = std::env::var("HSHARE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(PORT);
    start_on(port, downloads_dir(), wake)
}

fn start_on(
    port: u16,
    inbox: PathBuf,
    wake: impl Fn() + Send + Sync + 'static,
) -> (Arc<Net>, std::sync::mpsc::Receiver<Event>) {
    let (tx, rx) = channel();
    let net = Arc::new(Net {
        name: hostname(),
        code: random_code(),
        port,
        events: Mutex::new(tx),
        wake: Box::new(wake),
        viewing: AtomicBool::new(false),
        casting: AtomicBool::new(false),
        inbox,
    });
    match TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => {
            let n = net.clone();
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    let n = n.clone();
                    std::thread::spawn(move || {
                        let peer = conn.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
                        if let Err(e) = serve(&n, conn) {
                            eprintln!("hshare: session from {peer}: {e}");
                        }
                    });
                }
            });
        }
        Err(e) => net.send(Event::Error(crate::tf!(
            "Impossible d'écouter sur le port {port} : {e}. Une autre copie de hShare est-elle ouverte ?",
            "Cannot listen on port {port}: {e}. Is another hShare already open?"
        ))),
    }
    // Discovery: a second instance on the same machine (tests) simply can't be found by code.
    if let Ok(sock) = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
        let n = net.clone();
        std::thread::spawn(move || answer_discovery(&n, sock));
    }
    (net, rx)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "hShare".into())
}

fn random_code() -> String {
    use std::hash::{BuildHasher, Hasher};
    let n = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{:06}", n % 1_000_000)
}

/// The local IPv4 address other machines reach us on (the one with the default route).
pub fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET address: no packet is sent
    Some(s.local_addr().ok()?.ip())
}

fn answer_discovery(net: &Net, sock: UdpSocket) {
    let mut buf = [0u8; 64];
    while let Ok((n, from)) = sock.recv_from(&mut buf) {
        let msg = String::from_utf8_lossy(&buf[..n]);
        if msg.strip_prefix("HSHARE?") == Some(net.code.as_str()) {
            let _ = sock.send_to(format!("HSHARE!{} {}", net.port, net.name).as_bytes(), from);
        }
    }
}

/// Parses what the user typed: `482913`, `482 913`, or `192.168.1.20 482913`
/// (optionally `ip:port code`). Returns the address if given, and the code.
pub fn parse_target(s: &str) -> Result<(Option<SocketAddr>, String), String> {
    let mut addr = None;
    let mut code = String::new();
    for word in s.split_whitespace() {
        if word.contains('.') || word.contains(':') {
            addr = Some(
                word.parse::<SocketAddr>()
                    .or_else(|_| word.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, PORT)))
                    .map_err(|_| crate::tf!("Adresse invalide : {word}", "Invalid address: {word}"))?,
            );
        } else {
            code.push_str(word);
        }
    }
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(crate::i18n::tr(
            "Entrez le code à 6 chiffres affiché sur l'autre appareil.",
            "Enter the 6-digit code shown on the other device.",
        )
        .into());
    }
    Ok((addr, code))
}

/// Finds the instance showing `code` by UDP broadcast.
fn discover(code: &str) -> io::Result<(SocketAddr, String)> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(700)))?;
    let mut buf = [0u8; 256];
    for _ in 0..3 {
        sock.send_to(format!("HSHARE?{code}").as_bytes(), ("255.255.255.255", DISCOVERY_PORT))?;
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            let msg = String::from_utf8_lossy(&buf[..n]);
            if let Some((port, name)) = msg.strip_prefix("HSHARE!").and_then(|m| m.split_once(' '))
                && let Ok(port) = port.parse()
            {
                return Ok((SocketAddr::new(from.ip(), port), name.to_string()));
            }
        }
    }
    Err(err(crate::i18n::tr(
        "Aucun appareil avec ce code sur le réseau. Vérifiez le code, ou ajoutez son adresse IP avant le code.",
        "No device with this code on the network. Check the code, or type its IP address before the code.",
    )))
}

/// Connects to the target and opens a session; returns the stream and the peer's name.
fn connect(net: &Net, target: &str, purpose: Purpose) -> io::Result<(TcpStream, String)> {
    let (addr, code) = parse_target(target).map_err(err)?;
    let (addr, name) = match addr {
        Some(a) => (a, a.ip().to_string()),
        None => discover(&code)?,
    };
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_nodelay(true)?;
    let hello = Hello { name: net.name.clone(), code, purpose };
    write_msg(&mut s, HELLO, &serde_json::to_vec(&hello)?)?;
    match read_msg(&mut s)? {
        (ACCEPT, _) => Ok((s, name)),
        (REJECT, why) => Err(err(String::from_utf8_lossy(&why))),
        _ => Err(err("unexpected answer")),
    }
}

/// Handles one incoming connection.
fn serve(net: &Net, mut s: TcpStream) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (kind, body) = read_msg(&mut s)?;
    let hello: Hello = if kind == HELLO { serde_json::from_slice(&body)? } else { return Err(err("no hello")) };
    if hello.code != net.code {
        return write_msg(&mut s, REJECT, crate::i18n::tr("Code incorrect.", "Wrong code.").as_bytes());
    }
    let peer = hello.name;
    match hello.purpose {
        Purpose::Cast => {
            if net.viewing.swap(true, Ordering::SeqCst) {
                return write_msg(&mut s, REJECT, crate::i18n::tr("L'écran est déjà utilisé.", "The screen is already in use.").as_bytes());
            }
            write_msg(&mut s, ACCEPT, &[])?;
            s.set_read_timeout(Some(Duration::from_secs(15)))?;
            view(net, s, peer);
            Ok(())
        }
        Purpose::Reverse => {
            if net.casting.load(Ordering::SeqCst) {
                return write_msg(&mut s, REJECT, crate::i18n::tr("Cet écran est déjà partagé.", "This screen is already being shared.").as_bytes());
            }
            let (reply, answer) = channel();
            net.send(Event::ReverseRequest { peer: peer.clone(), reply });
            if answer.recv_timeout(Duration::from_secs(60)) != Ok(true) {
                return write_msg(&mut s, REJECT, crate::i18n::tr("Demande refusée.", "Request declined.").as_bytes());
            }
            if net.casting.swap(true, Ordering::SeqCst) {
                return write_msg(&mut s, REJECT, crate::i18n::tr("Cet écran est déjà partagé.", "This screen is already being shared.").as_bytes());
            }
            write_msg(&mut s, ACCEPT, &[])?;
            stream_screen(net, s, peer);
            Ok(())
        }
        Purpose::File => {
            write_msg(&mut s, ACCEPT, &[])?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            let (kind, body) = read_msg(&mut s)?;
            let head: FileHeader = if kind == FILE { serde_json::from_slice(&body)? } else { return Err(err("no file header")) };
            let path = unique_path(&net.inbox, &head.name);
            let mut out = std::fs::File::create(&path)?;
            let copied = io::copy(&mut (&mut s).take(head.size), &mut out)?;
            if copied != head.size {
                drop(out);
                let _ = std::fs::remove_file(&path);
                return Err(err("file transfer interrupted"));
            }
            write_msg(&mut s, ACCEPT, &[])?;
            net.send(Event::FileReceived { path, from: peer });
            Ok(())
        }
    }
}

/// Reads frames from `s` and passes them to the UI until either side stops.
fn view(net: &Net, mut s: TcpStream, peer: String) {
    net.send(Event::ViewStarted { peer: peer.clone() });
    let error = loop {
        if !net.viewing.load(Ordering::SeqCst) {
            break None; // stopped here
        }
        match read_msg(&mut s) {
            Ok((FRAME, jpeg)) => match decode(&jpeg) {
                Ok(img) => net.send(Event::Frame(img)),
                Err(e) => break Some(e),
            },
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break None, // the other side stopped
            Err(e) => break Some(e.to_string()),
        }
    };
    let _ = s.shutdown(std::net::Shutdown::Both);
    net.viewing.store(false, Ordering::SeqCst);
    net.send(Event::ViewEnded { peer, error });
}

fn decode(jpeg: &[u8]) -> Result<Image, String> {
    let mut d = jpeg_decoder::Decoder::new(jpeg);
    let rgb = d.decode().map_err(|e| e.to_string())?;
    let info = d.info().ok_or("no image info")?;
    if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return Err("unexpected JPEG pixel format".into());
    }
    Ok(Image { width: info.width.into(), height: info.height.into(), rgb })
}

/// Captures this screen and sends it on `s` until stopped or disconnected.
fn stream_screen(net: &Net, mut s: TcpStream, peer: String) {
    let stop = Arc::new(AtomicBool::new(false));
    let frames = crate::capture::start(stop.clone());
    let mut started = false;
    let error = loop {
        if !net.casting.load(Ordering::SeqCst) {
            break None;
        }
        let Some(frame) = frames.take(Duration::from_millis(250)) else { continue };
        let jpeg = match frame.and_then(|f| crate::capture::encode(&f)) {
            Ok(j) => j,
            Err(e) => break Some(e),
        };
        if !started {
            started = true;
            net.send(Event::CastStarted { peer: peer.clone() });
        }
        if let Err(e) = write_msg(&mut s, FRAME, &jpeg) {
            break (e.kind() != io::ErrorKind::BrokenPipe && e.kind() != io::ErrorKind::ConnectionReset)
                .then(|| e.to_string());
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = s.shutdown(std::net::Shutdown::Both);
    net.casting.store(false, Ordering::SeqCst);
    net.send(Event::CastEnded { error });
}

/// Shares this screen with `target` (runs on its own thread).
pub fn cast(net: Arc<Net>, target: String) {
    net.casting.store(true, Ordering::SeqCst);
    std::thread::spawn(move || match connect(&net, &target, Purpose::Cast) {
        Ok((s, peer)) => stream_screen(&net, s, peer),
        Err(e) => {
            net.casting.store(false, Ordering::SeqCst);
            net.send(Event::CastEnded { error: Some(e.to_string()) });
        }
    });
}

/// Asks `target` to show us its screen.
pub fn reverse(net: Arc<Net>, target: String) {
    net.viewing.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        // The other user has up to a minute to accept.
        match connect(&net, &target, Purpose::Reverse).and_then(|(s, p)| {
            s.set_read_timeout(Some(Duration::from_secs(70)))?;
            Ok((s, p))
        }) {
            Ok((s, peer)) => view(&net, s, peer),
            Err(e) => {
                net.viewing.store(false, Ordering::SeqCst);
                net.send(Event::ViewEnded { peer: String::new(), error: Some(e.to_string()) });
            }
        }
    });
}

/// Sends a file to `target`.
pub fn send_file(net: Arc<Net>, target: String, path: PathBuf) {
    std::thread::spawn(move || {
        let result = (|| {
            let mut file = std::fs::File::open(&path)?;
            let size = file.metadata()?.len();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let (mut s, peer) = connect(&net, &target, Purpose::File)?;
            write_msg(&mut s, FILE, &serde_json::to_vec(&FileHeader { name: name.clone(), size })?)?;
            io::copy(&mut file, &mut s)?;
            s.flush()?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            match read_msg(&mut s)? {
                (ACCEPT, _) => Ok((name, peer)),
                _ => Err(err("the receiver did not confirm the file")),
            }
        })();
        net.send(match result {
            Ok((name, to)) => Event::FileSent { name, to },
            Err(e) => Event::Error(crate::tf!("Envoi impossible : {e}", "Could not send the file: {e}")),
        });
    });
}

pub fn downloads_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
    // XDG_DOWNLOAD_DIR from user-dirs.dirs, e.g. "$HOME/Téléchargements".
    let xdg = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("XDG_DOWNLOAD_DIR="))
            .map(|v| v.trim_matches('"').replace("$HOME", &home.to_string_lossy()))
    });
    let dir = xdg.map(PathBuf::from).unwrap_or_else(|| home.join("Downloads"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `dir/name`, or `dir/name (2).ext`... if taken. Only the base name of `name` is kept,
/// so a sender cannot write outside `dir`.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base = Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned());
    let base = base.filter(|b| !b.starts_with('.')).unwrap_or_else(|| "file".into());
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (base.clone(), String::new()),
    };
    let mut path = dir.join(&base);
    let mut i = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({i}){ext}"));
        i += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        assert_eq!(parse_target("482913").unwrap(), (None, "482913".into()));
        assert_eq!(parse_target(" 482 913 ").unwrap(), (None, "482913".into()));
        let (a, c) = parse_target("192.168.1.20 482913").unwrap();
        assert_eq!((a.unwrap().to_string(), c), ("192.168.1.20:47800".into(), "482913".into()));
        let (a, _) = parse_target("127.0.0.1:47810 000001").unwrap();
        assert_eq!(a.unwrap().port(), 47810);
        assert!(parse_target("12345").is_err());
        assert!(parse_target("abcdef").is_err());
        assert!(parse_target("1.2.3 482913").is_err());
    }

    #[test]
    fn received_files_stay_in_the_folder() {
        let dir = std::env::temp_dir().join(format!("hshare-test-{}", random_code()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "../../etc/passwd"), dir.join("passwd"));
        assert_eq!(unique_path(&dir, ".bashrc"), dir.join("file"));
        std::fs::write(dir.join("a.pdf"), "x").unwrap();
        assert_eq!(unique_path(&dir, "a.pdf"), dir.join("a (2).pdf"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two instances on test ports: a file goes through, a wrong code and a declined
    /// reverse request are refused.
    #[test]
    fn sessions_between_two_instances() {
        let dir = std::env::temp_dir().join(format!("hshare-net-{}", random_code()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, a_events) = start_on(47870, dir.join("a"), || {});
        let (b, _) = start_on(47871, dir.join("b"), || {});
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let target = format!("127.0.0.1:47870 {}", a.code);

        let doc = dir.join("notes.txt");
        std::fs::write(&doc, "bonjour").unwrap();
        send_file(b.clone(), target.clone(), doc);
        match a_events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::FileReceived { path, .. } => {
                assert_eq!(path, dir.join("a/notes.txt"));
                assert_eq!(std::fs::read_to_string(path).unwrap(), "bonjour");
            }
            _ => panic!("expected FileReceived"),
        }

        let wrong = if a.code == "000000" { "111111" } else { "000000" };
        let e = connect(&b, &format!("127.0.0.1:47870 {wrong}"), Purpose::Cast).unwrap_err();
        assert!(e.to_string().contains("Code"), "{e}");

        let asker = std::thread::spawn({
            let b = b.clone();
            move || connect(&b, &target, Purpose::Reverse).map(|_| ())
        });
        match a_events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::ReverseRequest { reply, .. } => reply.send(false).unwrap(),
            _ => panic!("expected ReverseRequest"),
        }
        assert!(asker.join().unwrap().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn messages_round_trip() {
        let mut buf = Vec::new();
        write_msg(&mut buf, FRAME, b"jpeg").unwrap();
        assert_eq!(read_msg(&mut &buf[..]).unwrap(), (FRAME, b"jpeg".to_vec()));
        let mut huge = vec![FRAME];
        huge.extend(u32::MAX.to_be_bytes());
        assert!(read_msg(&mut &huge[..]).is_err());
    }
}
