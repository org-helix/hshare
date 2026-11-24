use crate::{config_dir, copy, mime, notify, Config, Res};
use base64::Engine;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const IMGUR_CLIENT_ID: &str = "8a98f183fc895da";

pub fn list() -> Res<Vec<String>> {
    let mut names = vec!["imgur".to_string(), "clipboard".to_string()];
    for (_, u) in custom_uploaders() {
        names.push(u?.name);
    }
    Ok(names)
}

pub fn upload(cfg: &Config, path: &Path, ext: &str) -> Res<()> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mime = mime(ext);
    match cfg.uploader.as_str() {
        "clipboard" => {
            copy(&data, mime)?;
            notify("hShare", "Copied to clipboard!");
        }
        "imgur" => imgur(cfg, data, mime)?,
        name => {
            let u = custom_uploaders()
                .into_iter()
                .filter_map(|(_, u)| u.ok())
                .find(|u| u.name == name)
                .ok_or(format!("no uploader named '{name}', see 'hshare uploaders'"))?;
            let link = u.upload(data, mime)?;
            println!("{link}");
            copy(link.as_bytes(), "text/plain")?;
            notify(&format!("hShare - {}", u.name), "Copied upload link to clipboard!");
        }
    }
    println!("{}", path.display());
    Ok(())
}

fn imgur(cfg: &Config, data: Vec<u8>, mime: &str) -> Res<()> {
    if data.len() > 10_000_000 {
        return Err("imgur upload failed: file over 10 MB".into());
    }
    let auth = match &cfg.imgur_token {
        Some(t) => format!("Bearer {t}"),
        None => format!("Client-ID {IMGUR_CLIENT_ID}"),
    };
    let headers = vec![("Content-Type".to_string(), mime.to_string()), ("Authorization".into(), auth)];
    let (status, body) = post("https://api.imgur.com/3/image", &headers, &data)?;
    let res: Value = serde_json::from_str(&body).unwrap_or_default();
    match res["data"]["link"].as_str() {
        Some(link) => {
            println!("{link}");
            copy(link.as_bytes(), "text/plain")?;
            notify("hShare - imgur", "Uploaded to imgur, link copied!");
            Ok(())
        }
        None => Err(format!("imgur upload failed: HTTP {status}: {}", res["data"]["error"])),
    }
}

/// POST that returns the status and body for any HTTP status.
fn post(url: &str, headers: &[(String, String)], body: &[u8]) -> Res<(u16, String)> {
    let mut req = ureq::post(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = match req.send_bytes(body) {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("upload to {url} failed: {e}")),
    };
    let status = resp.status();
    Ok((status, resp.into_string().map_err(|e| e.to_string())?))
}

fn custom_uploaders() -> Vec<(PathBuf, Res<Custom>)> {
    let Ok(dir) = fs::read_dir(config_dir().join("uploaders")) else { return vec![] };
    dir.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "uploader"))
        .map(|p| {
            let u = fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|s| Custom::parse(&s))
                .map_err(|e| format!("invalid uploader {}: {e}", p.display()));
            (p, u)
        })
        .collect()
}

/// A KShare-compatible `.uploader` JSON definition.
#[derive(Debug)]
struct Custom {
    name: String,
    target: String,
    format: String,
    body: Value,
    headers: Map<String, Value>,
    ret: String,
    limit: Option<f64>,
    base64: bool,
}

impl Custom {
    fn parse(src: &str) -> Res<Custom> {
        let o: Map<String, Value> = serde_json::from_str(src).map_err(|e| e.to_string())?;
        let s = |k: &str| o.get(k).and_then(Value::as_str);
        let name = s("name").ok_or("name is not a string")?.to_string();
        if !matches!(s("method").map(str::to_lowercase).as_deref(), None | Some("post")) {
            return Err("method invalid, only POST is supported".into());
        }
        let target = s("target").ok_or("target missing")?.to_string();
        if !target.starts_with("http://") && !target.starts_with("https://") {
            return Err("target not an http(s) URL".into());
        }
        let format = s("format").unwrap_or("json").to_lowercase();
        let body = o.get("body").cloned().unwrap_or(Value::Null);
        let body_ok = match format.as_str() {
            "plain" => true,
            "json" => body.is_object() || body.is_string(),
            "x-www-form-urlencoded" => body.is_object(),
            "multipart-form-data" => body.as_array().is_some_and(|a| a.iter().all(Value::is_object)),
            _ => return Err("format invalid".into()),
        };
        if !body_ok {
            return Err(format!("body has the wrong type for format {format}"));
        }
        let base64 = o.get("base64").and_then(Value::as_bool).unwrap_or(false);
        if format == "json" && !base64 {
            return Err("base64 required with json".into());
        }
        Ok(Custom {
            name,
            target,
            format,
            body,
            headers: o.get("headers").and_then(Value::as_object).cloned().unwrap_or_default(),
            ret: s("return").ok_or("return invalid")?.to_string(),
            limit: o.get("fileLimit").and_then(Value::as_f64),
            base64,
        })
    }

    fn upload(&self, mut data: Vec<u8>, mime: &str) -> Res<String> {
        if self.base64 {
            data = base64::engine::general_purpose::STANDARD.encode(&data).into_bytes();
        }
        let boundary = format!("hshare{:x}", std::time::UNIX_EPOCH.elapsed().unwrap_or_default().as_nanos());
        let (ctype, body) = match self.format.as_str() {
            "plain" => (mime.to_string(), data),
            "json" => {
                let body = match &self.body {
                    Value::String(s) => fill(&format!("/{s}/"), &data, mime),
                    v => serde_json::to_vec(&replace(v, &data, mime)).unwrap(),
                };
                ("application/json".into(), body)
            }
            "x-www-form-urlencoded" => {
                let pairs: Vec<String> = self.body.as_object().unwrap().iter().map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => fill(s, &data, mime),
                        v => v.to_string().into_bytes(),
                    };
                    format!("{}={}", urlencode(k.as_bytes()), urlencode(&v))
                }).collect();
                ("application/x-www-form-urlencoded".into(), pairs.join("&").into_bytes())
            }
            _ => {
                let mut out = Vec::new();
                for part in self.body.as_array().unwrap() {
                    out.extend(format!("--{boundary}\r\n").into_bytes());
                    for (k, v) in part.as_object().unwrap() {
                        if let (Some(h), Some(v)) = (k.strip_prefix("__"), v.as_str()) {
                            out.extend(format!("{h}: ").into_bytes());
                            out.extend(fill(v, b"", mime));
                            out.extend(b"\r\n");
                        }
                    }
                    out.extend(b"\r\n");
                    out.extend(match &part["body"] {
                        Value::String(s) => fill(s, &data, mime),
                        v => serde_json::to_vec(&replace(v, &data, mime)).unwrap(),
                    });
                    out.extend(b"\r\n");
                }
                out.extend(format!("--{boundary}--\r\n").into_bytes());
                (format!("multipart/form-data; boundary={boundary}"), out)
            }
        };
        if self.limit.is_some_and(|l| l > 0.0 && body.len() as f64 > l) {
            return Err(format!("{}: file limit exceeded", self.name));
        }
        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("content-type"))
            .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), String::from)))
            .collect();
        headers.push(("Content-Type".into(), ctype));
        let (status, resp) = post(&self.target, &headers, &body)?;
        if !(200..300).contains(&status) {
            return Err(format!("{}: HTTP {status}: {resp}", self.name));
        }
        if self.ret == "|" {
            return Ok(resp);
        }
        let json: Value = serde_json::from_str(&resp).map_err(|_| format!("{}: response is not JSON: {resp}", self.name))?;
        pathspec(&json, &self.ret).ok_or(format!("{}: '{}' not found in response: {resp}", self.name, self.ret))
    }
}

/// Strings wrapped in `/.../` get `%contenttype` and `%imagedata` substituted; others are literal.
fn fill(s: &str, data: &[u8], mime: &str) -> Vec<u8> {
    let Some(inner) = s.strip_prefix('/').and_then(|s| s.strip_suffix('/')) else {
        return s.as_bytes().to_vec();
    };
    let inner = inner.replace("%contenttype", mime);
    let mut out = Vec::new();
    for (i, piece) in inner.split("%imagedata").enumerate() {
        if i > 0 {
            out.extend_from_slice(data);
        }
        out.extend_from_slice(piece.as_bytes());
    }
    out
}

fn replace(v: &Value, data: &[u8], mime: &str) -> Value {
    match v {
        Value::String(s) => Value::String(String::from_utf8_lossy(&fill(s, data, mime)).into_owned()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), replace(v, data, mime))).collect()),
        v => v.clone(),
    }
}

/// `.a.b` walks into the JSON response; `.` returns all of it.
fn pathspec(json: &Value, spec: &str) -> Option<String> {
    let path = spec.strip_prefix('.')?;
    let mut v = json;
    for field in path.split('.').filter(|f| !f.is_empty()) {
        v = v.get(field)?;
    }
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        v => Some(v.to_string()),
    }
}

fn urlencode(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (c as char).to_string(),
            c => format!("%{c:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitution() {
        assert_eq!(fill("/a %imagedata b %contenttype/", b"XY", "image/png"), b"a XY b image/png");
        assert_eq!(fill("literal %imagedata", b"XY", "m"), b"literal %imagedata");
        assert_eq!(replace(&json!({"x": {"y": "/%imagedata/"}, "n": 1}), b"Z", "m"), json!({"x": {"y": "Z"}, "n": 1}));
    }

    #[test]
    fn response_pathspec() {
        let r = json!({"data": {"link": "https://x/y", "id": 3}});
        assert_eq!(pathspec(&r, ".data.link").as_deref(), Some("https://x/y"));
        assert_eq!(pathspec(&r, ".data.id").as_deref(), Some("3"));
        assert_eq!(pathspec(&r, ".nope"), None);
        assert_eq!(pathspec(&r, "data"), None);
        assert!(pathspec(&r, ".").unwrap().starts_with('{'));
    }

    #[test]
    fn parse_uploader() {
        let u = Custom::parse(r#"{"name":"t","target":"https://e.com","format":"multipart-form-data",
            "body":[{"__Content-Disposition":"form-data; name=\"f\"","body":"/%imagedata/"}],"return":".url"}"#).unwrap();
        assert_eq!(u.name, "t");
        assert!(Custom::parse(r#"{"name":"t","target":"ftp://e","return":"|"}"#).is_err());
        assert_eq!(urlencode(b"a+b/="), "a%2Bb%2F%3D");
    }
}
