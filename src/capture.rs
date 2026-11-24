//! Continuous screen capture.
//!
//! - Wayland (and X11 desktops with a portal): the desktop's screen-cast portal asks the
//!   user which screen or window to share, then frames arrive through PipeWire.
//! - Plain X11 without a portal session: the root window is read directly.
//!
//! Frames go into a [`Slot`] that only keeps the newest one, so a slow network drops
//! frames instead of building up delay.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// One captured frame, 4 bytes per pixel.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// Byte order of a pixel: true for B,G,R,x (most compositors), false for R,G,B,x.
    pub bgr: bool,
    pub data: Vec<u8>,
}

/// The newest frame, waiting for the sender.
#[derive(Default)]
pub struct Slot {
    frame: Mutex<Option<Result<Frame, String>>>,
    ready: Condvar,
}

impl Slot {
    fn put(&self, f: Result<Frame, String>) {
        *self.frame.lock().unwrap() = Some(f);
        self.ready.notify_one();
    }

    /// Waits up to `timeout` for a new frame (or a capture error).
    pub fn take(&self, timeout: Duration) -> Option<Result<Frame, String>> {
        let guard = self.frame.lock().unwrap();
        let (mut guard, _) = self.ready.wait_timeout_while(guard, timeout, |f| f.is_none()).unwrap();
        guard.take()
    }
}

/// Starts capturing on a background thread until `stop` is set.
pub fn start(stop: Arc<AtomicBool>) -> Arc<Slot> {
    let slot = Arc::new(Slot::default());
    let out = slot.clone();
    std::thread::spawn(move || {
        let x11_only = std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some();
        let result = if x11_only { x11(&out, &stop) } else { portal(&out, &stop) };
        if let Err(e) = result {
            out.put(Err(e));
        }
    });
    slot
}

fn x11(slot: &Slot, stop: &AtomicBool) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = &conn.setup().roots[screen];
    let (w, h) = (root.width_in_pixels, root.height_in_pixels);
    while !stop.load(Ordering::Relaxed) {
        let img = conn
            .get_image(ImageFormat::Z_PIXMAP, root.root, 0, 0, w, h, !0)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        slot.put(Ok(Frame { width: w.into(), height: h.into(), stride: w as usize * 4, bgr: true, data: img.data }));
        std::thread::sleep(Duration::from_millis(66)); // ~15 fps
    }
    Ok(())
}

#[cfg(not(feature = "portal"))]
fn portal(_: &Arc<Slot>, _: &Arc<AtomicBool>) -> Result<(), String> {
    Err("hShare was built without Wayland capture (the \"portal\" feature)".into())
}

#[cfg(feature = "portal")]
fn portal(slot: &Arc<Slot>, stop: &Arc<AtomicBool>) -> Result<(), String> {
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    // The session must stay alive while streaming: dropping it ends the cast.
    let (_proxy, _session, node, fd) = rt
        .block_on(async {
            let proxy = Screencast::new().await?;
            let session = proxy.create_session(Default::default()).await?;
            proxy
                .select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(CursorMode::EMBEDDED)
                        .set_sources(SourceType::MONITOR | SourceType::WINDOW)
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::DoNot),
                )
                .await?;
            let streams = proxy.start(&session, None, Default::default()).await?.response()?;
            let node = streams.streams().first().map(|s| s.pipe_wire_node_id());
            let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
            Ok::<_, ashpd::Error>((proxy, session, node, fd))
        })
        .map_err(|e| match e {
            ashpd::Error::Response(_) => crate::i18n::tr("Partage annulé.", "Sharing cancelled.").to_string(),
            e => e.to_string(),
        })?;
    let node = node.ok_or("the portal returned no stream")?;
    pipewire_stream(fd, node, slot.clone(), stop.clone()).map_err(|e| format!("PipeWire: {e}"))
}

#[cfg(feature = "portal")]
// The PipeWire callbacks must own what they use.
fn pipewire_stream(fd: std::os::fd::OwnedFd, node: u32, slot: Arc<Slot>, stop: Arc<AtomicBool>) -> Result<(), pipewire::Error> {
    use pipewire as pw;
    use pw::spa;
    use pw::spa::param::video::VideoFormat;

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "hshare",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(spa::param::video::VideoInfoRaw::default())
        .param_changed(|_, info, id, param| {
            if let Some(param) = param
                && id == spa::param::ParamType::Format.as_raw()
            {
                let _ = info.parse(param);
            }
        })
        .process(move |stream, info| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let (size, stride) = (d.chunk().size() as usize, d.chunk().stride());
            let (w, h) = (info.size().width, info.size().height);
            let Some(bytes) = d.data() else { return };
            if w == 0 || size == 0 || bytes.len() < size {
                return;
            }
            let stride = if stride > 0 { stride as usize } else { w as usize * 4 };
            let bgr = matches!(info.format(), VideoFormat::BGRx | VideoFormat::BGRA);
            slot.put(Ok(Frame { width: w, height: h, stride, bgr, data: bytes[..size].to_vec() }));
        })
        .register()?;

    let rect = |width, height| spa::utils::Rectangle { width, height };
    let fps = |num| spa::utils::Fraction { num, denom: 1 };
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Video),
        spa::pod::property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Raw),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice, Enum, Id,
            VideoFormat::BGRx, VideoFormat::BGRx, VideoFormat::BGRA, VideoFormat::RGBx, VideoFormat::RGBA
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice, Range, Rectangle,
            rect(1920, 1080), rect(1, 1), rect(8192, 8192)
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice, Range, Fraction,
            fps(30), fps(0), fps(60)
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .expect("static format pod serializes")
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).expect("serialized pod parses")];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    // Quit the loop once the cast is stopped from the UI or the network.
    let ml = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop.load(Ordering::Relaxed) {
            ml.quit();
        }
    });
    let tick = Some(Duration::from_millis(100));
    let _ = timer.update_timer(tick, tick);
    mainloop.run();
    Ok(())
}

/// JPEG-encodes a frame, halving its size when wider than 2560 px.
pub fn encode(f: &Frame) -> Result<Vec<u8>, String> {
    let step = if f.width > 2560 { 2 } else { 1 };
    let (w, h) = (f.width as usize / step, f.height as usize / step);
    let mut packed = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &f.data[y * step * f.stride..];
        if step == 1 {
            packed.extend_from_slice(&row[..w * 4]);
        } else {
            for x in 0..w {
                packed.extend_from_slice(&row[x * 8..x * 8 + 4]);
            }
        }
    }
    let mut out = Vec::new();
    let color = if f.bgr { jpeg_encoder::ColorType::Bgra } else { jpeg_encoder::ColorType::Rgba };
    jpeg_encoder::Encoder::new(&mut out, 70)
        .encode(&packed, w as u16, h as u16, color)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_a_padded_bgr_frame() {
        // 4x2 frame with 8 bytes of row padding; pixel (0,0) blue, the rest red.
        let (w, h, stride) = (4u32, 2u32, 24usize);
        let mut data = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = &mut data[y * stride + x * 4..][..4];
                p.copy_from_slice(if x == 0 && y == 0 { &[255, 0, 0, 0] } else { &[0, 0, 255, 0] });
            }
        }
        let jpeg = encode(&Frame { width: w, height: h, stride, bgr: true, data }).unwrap();
        let mut dec = jpeg_decoder::Decoder::new(&jpeg[..]);
        let rgb = dec.decode().unwrap();
        let info = dec.info().unwrap();
        assert_eq!((info.width, info.height), (4, 2));
        assert!(rgb[3 * 3] > 150 && rgb[3 * 3 + 2] < 100, "pixel (3,0) should be red");
    }
}
