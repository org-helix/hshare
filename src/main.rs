mod upload;

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};

type Res<T> = Result<T, String>;

const HELP: &str = "hshare - screenshot, record and upload on Linux

Usage: hshare [-u <uploader>] <command>

Commands:
  full              capture all screens
  area              select a region and capture it
  window            capture the active window
  record [area|full|window]
                    start a recording; run it again to stop and upload
  upload <file>     upload an existing file
  uploaders         list available uploaders

Options:
  -u <uploader>     override the uploader from the config (imgur, clipboard, or a custom one)
  -h, --help        show this help
  -V, --version     print the version

Config: ~/.config/hshare/config.json
Custom uploaders: ~/.config/hshare/uploaders/*.uploader (KShare format)
Bind the commands to keys in your desktop's shortcut settings.";

fn main() {
    if let Err(e) = run(env::args().skip(1).collect()) {
        eprintln!("hshare: {e}");
        notify("hShare error", &e);
        std::process::exit(1);
    }
}

fn run(mut args: Vec<String>) -> Res<()> {
    let mut cfg = Config::load()?;
    if let Some(i) = args.iter().position(|a| a == "-u") {
        args.remove(i);
        if i >= args.len() {
            return Err("-u needs an uploader name".into());
        }
        cfg.uploader = args.remove(i);
    }
    let arg = |n: usize| args.get(n).map(String::as_str);
    match arg(0) {
        Some("full" | "area" | "window") => {
            let ext = cfg.capture_format.clone();
            let path = cfg.output_path(&ext)?;
            capture(arg(0).unwrap(), &path)?;
            if !path.exists() {
                return Ok(()); // selection cancelled
            }
            upload::upload(&cfg, &path, &ext)
        }
        Some("record") => record(&cfg, arg(1).unwrap_or("area")),
        Some("upload") => {
            let path = PathBuf::from(arg(1).ok_or("upload needs a file")?);
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_lowercase();
            upload::upload(&cfg, &path, &ext)
        }
        Some("uploaders") => {
            for name in upload::list()? {
                println!("{name}");
            }
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("hshare {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        None | Some("-h" | "--help") => {
            println!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}', see --help")),
    }
}

pub struct Config {
    pub uploader: String,
    pub save_dir: PathBuf,
    pub file_format: String,
    pub capture_format: String,
    pub recording_format: String,
    pub imgur_token: Option<String>,
}

pub fn config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("hshare")
}

fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

impl Config {
    fn load() -> Res<Config> {
        let path = config_dir().join("config.json");
        let v: Value = match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(_) => Value::Null,
        };
        let s = |k: &str, d: &str| v[k].as_str().unwrap_or(d).to_string();
        let save_dir = s("save_dir", "~/Screenshots");
        let save_dir = match save_dir.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => PathBuf::from(save_dir),
        };
        Ok(Config {
            uploader: s("uploader", "imgur"),
            save_dir,
            file_format: s("file_format", "Screenshot %(yyyy-MM-dd HH:mm:ss)date.%ext"),
            capture_format: s("capture_format", "png").to_lowercase(),
            recording_format: s("recording_format", "webm").to_lowercase(),
            imgur_token: v["imgur_token"].as_str().map(String::from),
        })
    }

    fn output_path(&self, ext: &str) -> Res<PathBuf> {
        fs::create_dir_all(&self.save_dir).map_err(|e| format!("{}: {e}", self.save_dir.display()))?;
        Ok(self.save_dir.join(format_name(&self.file_format, ext, chrono::Local::now())))
    }
}

/// KShare file name format: `%(<Qt date format>)date` and `%ext`.
fn format_name(fmt: &str, ext: &str, now: chrono::DateTime<chrono::Local>) -> String {
    let mut out = fmt.to_string();
    if let (Some(a), Some(b)) = (fmt.find("%("), fmt.rfind(")date")) {
        if a < b {
            let date = now.format(&qt_to_strftime(&fmt[a + 2..b])).to_string();
            out = format!("{}{}{}", &fmt[..a], date, &fmt[b + 5..]);
        }
    }
    out.replace("%ext", ext).replace('/', "-")
}

fn qt_to_strftime(qt: &str) -> String {
    const TOKENS: [(&str, &str); 21] = [
        ("yyyy", "%Y"), ("yy", "%y"), ("MMMM", "%B"), ("MMM", "%b"), ("MM", "%m"), ("M", "%-m"),
        ("dddd", "%A"), ("ddd", "%a"), ("dd", "%d"), ("d", "%-d"), ("HH", "%H"), ("H", "%-H"),
        ("hh", "%I"), ("h", "%-I"), ("mm", "%M"), ("m", "%-M"), ("ss", "%S"), ("s", "%-S"),
        ("zzz", "%3f"), ("AP", "%p"), ("ap", "%P"),
    ];
    let mut out = String::new();
    let mut rest = qt;
    'outer: while let Some(c) = rest.chars().next() {
        for (t, s) in TOKENS {
            if let Some(r) = rest.strip_prefix(t) {
                out.push_str(s);
                rest = r;
                continue 'outer;
            }
        }
        if c == '%' {
            out.push('%');
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[derive(PartialEq)]
enum Backend {
    Spectacle, // KDE Plasma (X11 and Wayland)
    Grim,      // wlroots compositors: sway, Hyprland, river...
    Maim,      // X11
}

fn backend() -> Backend {
    let kde = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().contains("KDE");
    if kde && which("spectacle") {
        Backend::Spectacle
    } else if env::var_os("WAYLAND_DISPLAY").is_some() {
        Backend::Grim
    } else {
        Backend::Maim
    }
}

fn which(bin: &str) -> bool {
    env::var_os("PATH").is_some_and(|p| env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn wayland() -> bool {
    env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Runs a command and returns its stdout; a missing binary becomes an install hint.
pub fn sh(cmd: &[&str], stdin: Option<&[u8]>) -> Res<String> {
    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run '{}' ({e}), is it installed?", cmd[0]))?;
    if let Some(data) = stdin {
        child.stdin.take().unwrap().write_all(data).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("'{}' failed ({})", cmd.join(" "), out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn capture(mode: &str, path: &Path) -> Res<()> {
    let p = path.to_str().ok_or("non UTF-8 save path")?;
    let ext = if p.ends_with(".png") { "png" } else { "jpeg" };
    match (backend(), mode) {
        (Backend::Spectacle, _) => {
            let flag = match mode { "full" => "-f", "area" => "-r", _ => "-a" };
            sh(&["spectacle", "-b", "-n", flag, "-o", p], None)?;
        }
        (Backend::Grim, "full") => drop(sh(&["grim", "-t", ext, p], None)?),
        (Backend::Grim, "area") => match sh(&["slurp"], None) {
            Ok(geom) => drop(sh(&["grim", "-t", ext, "-g", &geom, p], None)?),
            Err(_) => return Ok(()), // slurp exits non-zero when cancelled
        },
        (Backend::Grim, _) => return Err("window capture needs KDE or X11, use 'area' instead".into()),
        (Backend::Maim, "full") => drop(sh(&["maim", "-f", ext, p], None)?),
        (Backend::Maim, "area") => {
            let _ = sh(&["maim", "-s", "-f", ext, p], None); // non-zero on cancel
        }
        (Backend::Maim, _) => {
            let win = sh(&["xdotool", "getactivewindow"], None)?;
            sh(&["maim", "-f", ext, "-i", &win, p], None)?;
        }
    }
    Ok(())
}

fn record(cfg: &Config, mode: &str) -> Res<()> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(env::temp_dir);
    let pidfile = runtime.join("hshare-record.pid");
    // A second invocation stops the running recording; the first one then uploads it.
    if let Ok(pid) = fs::read_to_string(&pidfile) {
        let pid = pid.trim();
        if !pid.is_empty() && Path::new("/proc").join(pid).exists() {
            return sh(&["kill", "-INT", pid], None).map(drop);
        }
    }
    let format = cfg.recording_format.clone();
    let final_path = cfg.output_path(&format)?;
    // Recorders write webm/mp4; gif is converted afterwards with ffmpeg.
    let rec_path = if format == "gif" { final_path.with_extension("mp4") } else { final_path.clone() };
    let rp = rec_path.to_str().ok_or("non UTF-8 save path")?.to_string();

    let mut cmd: Vec<String> = match backend() {
        // Spectacle can't record in background mode; KDE's portal dialog picks the screen or window.
        Backend::Spectacle => {
            let mut c: Vec<String> = ["gpu-screen-recorder", "-w", "portal", "-f", "60"].map(String::from).to_vec();
            if rec_path.extension().is_some_and(|e| e == "webm") {
                c.extend(["-k".into(), "vp9".into()]);
            }
            c.extend(["-o".into(), rp.clone()]);
            c
        }
        Backend::Grim => {
            let mut c = vec!["wf-recorder".into(), "-y".into(), "-f".into(), rp.clone()];
            if mode != "full" {
                let Ok(geom) = sh(&["slurp"], None) else { return Ok(()) };
                c.extend(["-g".into(), geom]);
            }
            c
        }
        Backend::Maim => {
            let display = env::var("DISPLAY").map_err(|_| "DISPLAY is not set")?;
            let mut c: Vec<String> = ["ffmpeg", "-y", "-loglevel", "error", "-f", "x11grab"].map(String::from).to_vec();
            if mode == "full" {
                c.extend(["-i".into(), display]);
            } else {
                let Ok(geom) = sh(&["slop", "-f", "%wx%h %x,%y"], None) else { return Ok(()) };
                let (size, off) = geom.split_once(' ').ok_or("unexpected slop output")?;
                c.extend(["-video_size".into(), size.into(), "-i".into(), format!("{display}+{off}")]);
            }
            c.push(rp.clone());
            c
        }
    };
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .spawn()
        .map_err(|e| format!("cannot run '{}' ({e}), is it installed?", cmd[0]))?;
    fs::write(&pidfile, child.id().to_string()).map_err(|e| e.to_string())?;
    let status = child.wait();
    let _ = fs::remove_file(&pidfile);
    status.map_err(|e| e.to_string())?;
    if !rec_path.exists() {
        return Err(format!("'{}' saved no recording", cmd[0]));
    }
    if format == "gif" {
        cmd = ["ffmpeg", "-y", "-loglevel", "error", "-i", &rp, "-vf",
            "fps=15,split[a][b];[a]palettegen[p];[b][p]paletteuse"].map(String::from).to_vec();
        cmd.push(final_path.to_str().unwrap().into());
        sh(&cmd.iter().map(String::as_str).collect::<Vec<_>>(), None)?;
        let _ = fs::remove_file(&rec_path);
    }
    upload::upload(cfg, &final_path, &format)
}

pub fn copy(data: &[u8], mime: &str) -> Res<()> {
    if wayland() {
        sh(&["wl-copy", "--type", mime], Some(data))
    } else {
        sh(&["xclip", "-selection", "clipboard", "-t", mime, "-i"], Some(data))
    }
    .map(drop)
}

pub fn notify(title: &str, body: &str) {
    let _ = sh(&["notify-send", "-a", "hShare", "-i", "camera-photo", title, body], None);
}

pub fn mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn file_name_format() {
        let t = chrono::Local.with_ymd_and_hms(2026, 3, 7, 9, 5, 2).unwrap();
        assert_eq!(
            format_name("Screenshot %(yyyy-MM-dd HH:mm:ss)date.%ext", "png", t),
            "Screenshot 2026-03-07 09:05:02.png"
        );
        assert_eq!(format_name("shot_%(d/M yy 100%)date.%ext", "gif", t), "shot_7-3 26 100%.gif");
        assert_eq!(format_name("plain.%ext", "jpg", t), "plain.jpg");
    }
}
