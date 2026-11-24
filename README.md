# hShare

Screenshot, record and upload from the Linux command line. A Rust rewrite of
[KShare](https://github.com/ArsenArsen/KShare) (itself inspired by [ShareX](https://getsharex.com/)).

hShare uses your desktop's own capture tools, so it works on Wayland and X11:

| Session | Screenshots | Recording | Clipboard |
|---|---|---|---|
| KDE Plasma | `spectacle` | [`gpu-screen-recorder`](https://git.dec05eba.com/gpu-screen-recorder/about/) | `wl-clipboard` / `xclip` |
| sway, Hyprland, other wlroots | `grim` + `slurp` | `wf-recorder` + `slurp` | `wl-clipboard` |
| X11 | `maim` + `xdotool` | `ffmpeg` + `slop` | `xclip` |

`notify-send` shows notifications, and GIF recordings need `ffmpeg`.

## Install

Download `hshare` from the [releases](https://github.com/org-helix/hshare/releases) page, or build it yourself:

```bash
cargo install --git https://github.com/org-helix/hshare
```

## Usage

```
hshare full              capture all screens
hshare area              select a region and capture it
hshare window            capture the active window (KDE, X11)
hshare record [area|full|window]
                         start recording; run it again to stop and upload
                         (on KDE, KDE's screen picker chooses what to record)
hshare upload <file>     upload an existing file
hshare uploaders         list uploaders
hshare -u clipboard area use another uploader this once
```

Each capture is saved to `~/Screenshots`, then uploaded. The link (or the image, for the
`clipboard` uploader) ends up in your clipboard.

Global hotkeys come from your desktop: bind these commands in System Settings → Shortcuts
(KDE), in your sway or Hyprland config, or in your window manager's keybindings.

## Configuration

`~/.config/hshare/config.json`. Every key is optional:

```json
{
  "uploader": "imgur",
  "save_dir": "~/Screenshots",
  "file_format": "Screenshot %(yyyy-MM-dd HH:mm:ss)date.%ext",
  "capture_format": "png",
  "recording_format": "webm",
  "imgur_token": null
}
```

- `uploader`: `imgur`, `clipboard`, or the `name` of a custom uploader.
- `capture_format`: `png` or `jpg`. `recording_format`: `webm`, `mp4` or `gif`.
- `imgur_token`: an imgur OAuth access token to upload to your account. Without one, uploads are anonymous.

### Custom uploaders

Put KShare-format `.uploader` files in `~/.config/hshare/uploaders/`. Example:

```json
{
  "name": "myhost",
  "target": "https://example.com/upload",
  "format": "multipart-form-data",
  "headers": { "Authorization": "secret" },
  "body": [{
    "__Content-Disposition": "form-data; name=\"file\"; filename=\"upload\"",
    "__Content-Type": "/%contenttype/",
    "body": "/%imagedata/"
  }],
  "return": ".data.url"
}
```

- `format`: `multipart-form-data`, `json` (requires `"base64": true`), `x-www-form-urlencoded` or `plain`.
- Strings wrapped in `/.../` have `%imagedata` and `%contenttype` replaced.
- `return`: a path such as `.data.url` into the JSON response, `.` for the whole response, or `|` for the raw body.
- Optional: `base64` (encode the file), `fileLimit` (max body size in bytes).

## License

MIT, see [LICENSE](LICENSE).
