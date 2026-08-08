// src/onvif/soap.rs
use super::types::Credentials;
use base64::prelude::*;
use sha1::{Digest, Sha1};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const HTTP_TIMEOUT: Duration = Duration::from_millis(4000);
pub(super) const NS_DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
pub(super) const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";

pub(super) fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false) // let SOAP faults (HTTP 500) come back as a body
        .build();
    ureq::Agent::new_with_config(config)
}

/// POST an authenticated SOAP request; return the body, or a fault reason.
pub(super) fn soap_call(
    agent: &ureq::Agent,
    uri: &str,
    action: &str,
    creds: &Credentials,
    created: &str,
    body_inner: &str,
) -> Result<String, String> {
    let security = security_header(creds, created);
    let envelope = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" "#,
            r#"xmlns:tds="http://www.onvif.org/ver10/device/wsdl" "#,
            r#"xmlns:trt="http://www.onvif.org/ver10/media/wsdl" "#,
            r#"xmlns:tt="http://www.onvif.org/ver10/schema" "#,
            r#"xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd" "#,
            r#"xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">"#,
            r#"<s:Header>{security}</s:Header><s:Body>{body}</s:Body></s:Envelope>"#
        ),
        security = security,
        body = body_inner,
    );

    let content_type = format!("application/soap+xml; charset=utf-8; action=\"{action}\"");
    let mut resp = agent
        .post(uri)
        .header("Content-Type", &content_type)
        .send(&envelope)
        .map_err(|e| format!("HTTP error: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read error: {e}"))?;

    if let Ok(doc) = roxmltree::Document::parse(&text) {
        if let Some(reason) = soap_fault(&doc) {
            return Err(reason);
        }
    }
    Ok(text)
}

fn soap_fault(doc: &roxmltree::Document) -> Option<String> {
    let fault = doc.descendants().find(|n| n.tag_name().name() == "Fault")?;
    let reason = fault
        .descendants()
        .find(|n| matches!(n.tag_name().name(), "Text" | "faultstring"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "request rejected (check username / password)".to_string());
    Some(reason)
}

// ---------------------------------------------------------------------------
// WS-Security UsernameToken (PasswordDigest) — SOAP auth only. HTTP Digest for
// the snapshot endpoint is a separate scheme entirely; see snapshot.rs.
// ---------------------------------------------------------------------------

fn security_header(creds: &Credentials, created: &str) -> String {
    let nonce = nonce_bytes();
    let nonce_b64 = BASE64_STANDARD.encode(nonce);

    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(created.as_bytes());
    hasher.update(creds.password.as_bytes());
    let digest = BASE64_STANDARD.encode(hasher.finalize());

    format!(
        concat!(
            r#"<wsse:Security><wsse:UsernameToken>"#,
            r#"<wsse:Username>{user}</wsse:Username>"#,
            r#"<wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{digest}</wsse:Password>"#,
            r#"<wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce}</wsse:Nonce>"#,
            r#"<wsu:Created>{created}</wsu:Created>"#,
            r#"</wsse:UsernameToken></wsse:Security>"#
        ),
        user = xml_escape(&creds.username),
        digest = digest,
        nonce = nonce_b64,
        created = created,
    )
}

/// 16 unpredictable-enough bytes for the nonce (SHA-1 of time+counter; no `rand`).
fn nonce_bytes() -> [u8; 16] {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = CTR.fetch_add(1, Ordering::Relaxed);

    let mut h = Sha1::new();
    h.update(nanos.to_le_bytes());
    h.update(c.to_le_bytes());
    let out = h.finalize();

    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&out[..16]);
    nonce
}

// ---------------------------------------------------------------------------
// Device clock -> ISO-8601 `Created` (every authenticated call needs this,
// since digest tokens are timestamped against the device's own clock)
// ---------------------------------------------------------------------------

pub(super) fn device_created(agent: &ureq::Agent, uri: &str) -> Option<String> {
    let body = concat!(
        r#"<?xml version="1.0" encoding="UTF-8"?>"#,
        r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" "#,
        r#"xmlns:tds="http://www.onvif.org/ver10/device/wsdl">"#,
        r#"<s:Body><tds:GetSystemDateAndTime/></s:Body></s:Envelope>"#
    );
    let mut resp = agent
        .post(uri)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .send(body)
        .ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let doc = roxmltree::Document::parse(&text).ok()?;

    let dt = doc
        .descendants()
        .find(|n| n.tag_name().name() == "UTCDateTime")?;
    let field = |name: &str| -> Option<u32> {
        dt.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse().ok())
    };
    let (y, mo, d) = (field("Year")?, field("Month")?, field("Day")?);
    let (h, mi, s) = (
        field("Hour").unwrap_or(0),
        field("Minute").unwrap_or(0),
        field("Second").unwrap_or(0),
    );
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"))
}

pub(super) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
