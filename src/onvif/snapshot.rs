// src/onvif/snapshot.rs

use super::soap::build_agent;
use super::types::{Credentials, Snapshot};
use base64::prelude::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn capture(snapshot_url: &str, creds: &Credentials) -> Result<Snapshot, String> {
    let bytes = fetch_snapshot(snapshot_url, creds)?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
    let rgba = img.to_rgba8();
    Ok(Snapshot {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

fn fetch_snapshot(url: &str, creds: &Credentials) -> Result<Vec<u8>, String> {
    let agent = build_agent();

    let mut resp = agent.get(url).call().map_err(|e| format!("HTTP: {e}"))?;

    if resp.status().as_u16() != 401 {
        let code = resp.status().as_u16();
        if code != 200 {
            return Err(format!("snapshot HTTP {code} (snapshot error)"));
        }
        return resp
            .body_mut()
            .read_to_vec()
            .map_err(|e| format!("read: {e}"));
    }

    let challenge = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| "401 without a WWW-Authenticate challenge".to_string())?;
    drop(resp);

    let auth = build_auth_header(&challenge, "GET", url, creds)?;

    // Bypass ureq entirely for the authenticated retry — see module docs.
    fetch_authenticated_raw(url, &auth)
}

fn build_auth_header(
    challenge: &str,
    method: &str,
    url: &str,
    creds: &Credentials,
) -> Result<String, String> {
    let scheme = challenge
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    if scheme == "basic" {
        let token = BASE64_STANDARD.encode(format!("{}:{}", creds.username, creds.password));
        return Ok(format!("Basic {token}"));
    }

    if scheme != "digest" {
        return Err(format!("unsupported auth scheme: {scheme}"));
    }

    let params = parse_auth_params(challenge);
    let realm = params.get("realm").cloned().unwrap_or_default();
    let nonce = params.get("nonce").cloned().unwrap_or_default();
    let opaque = params.get("opaque").cloned();
    let algorithm = params
        .get("algorithm")
        .cloned()
        .unwrap_or_else(|| "MD5".into());
    let use_qop = params
        .get("qop")
        .map(|q| q.split(',').any(|x| x.trim() == "auth"))
        .unwrap_or(false);

    let uri_path = request_target(url);
    let ha1 = md5_hex(&format!("{}:{realm}:{}", creds.username, creds.password));
    let ha2 = md5_hex(&format!("{method}:{uri_path}"));
    let cnonce = format!("{:016x}", simple_rand());
    let nc = "00000001";

    let response = if use_qop {
        md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"))
    } else {
        md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
    };

    let mut header = format!(
        r#"Digest username="{}", realm="{realm}", nonce="{nonce}", uri="{uri_path}", response="{response}", algorithm={algorithm}"#,
        creds.username
    );
    if use_qop {
        header.push_str(&format!(r#", qop=auth, nc={nc}, cnonce="{cnonce}""#));
    }
    if let Some(o) = opaque {
        header.push_str(&format!(r#", opaque="{o}""#));
    }
    Ok(header)
}

fn parse_auth_params(challenge: &str) -> std::collections::HashMap<String, String> {
    let after = challenge
        .splitn(2, char::is_whitespace)
        .nth(1)
        .unwrap_or("");
    let mut map = std::collections::HashMap::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut parts = Vec::new();
    for c in after.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ',' if !in_quotes => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            map.insert(
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches('"').to_string(),
            );
        }
    }
    map
}

/// The request-target (path + query) used in HA2 and the Authorization uri.
fn request_target(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.find('/').map(|i| rest[i..].to_string()))
        .unwrap_or_else(|| "/".to_string())
}

fn md5_hex(s: &str) -> String {
    use md5::{Digest, Md5};
    Md5::digest(s.as_bytes())
        .iter()
        .copied()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn simple_rand() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    static CTR: AtomicU64 = AtomicU64::new(0);
    nanos
        ^ CTR
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn fetch_authenticated_raw(url: &str, authorization: &str) -> Result<Vec<u8>, String> {
    let uri: ureq::http::Uri = url.parse().map_err(|e| format!("bad url: {e}"))?;
    let host = uri.host().ok_or("no host in url")?;
    let port = uri.port_u16().unwrap_or(80);
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");

    let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Authorization: {authorization}\r\n\
         User-Agent: camproto-odm\r\n\
         Accept: */*\r\n\
         Connection: close\r\n\
         \r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {e}"))?;

    let idx = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let header_text = String::from_utf8_lossy(&buf[..idx]);
    let status: u16 = header_text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status != 200 {
        return Err(format!("snapshot HTTP {status} (raw socket)"));
    }

    let mut body = buf[idx + 4..].to_vec();
    if header_text
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = dechunk(&body)?;
    }
    Ok(body)
}

fn dechunk(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let line_end = data[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk framing")?;
        let size_str =
            std::str::from_utf8(&data[pos..pos + line_end]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_str.trim(), 16).map_err(|_| "bad chunk size")?;
        pos += line_end + 2;
        if size == 0 {
            break;
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size + 2;
    }
    Ok(out)
}
