// src/onvif/media.rs

use super::soap::{soap_call, xml_escape, NS_MEDIA};
use super::types::{Credentials, MediaProfile};

pub(super) fn get_profiles(
    agent: &ureq::Agent,
    media_uri: &str,
    creds: &Credentials,
    created: &str,
) -> Result<Vec<MediaProfile>, String> {
    let action = format!("{NS_MEDIA}/GetProfiles");
    let resp = soap_call(
        agent,
        media_uri,
        &action,
        creds,
        created,
        "<trt:GetProfiles/>",
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    let mut out = Vec::new();
    for prof in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Profiles")
    {
        let token = prof.attribute("token").unwrap_or_default().to_string();
        // The profile's own Name is a direct child (VideoEncoderConfiguration
        // has a Name too, so match on children, not descendants).
        let name = prof
            .children()
            .find(|n| n.tag_name().name() == "Name")
            .and_then(|n| n.text())
            .unwrap_or_default()
            .to_string();

        let venc = prof
            .descendants()
            .find(|n| n.tag_name().name() == "VideoEncoderConfiguration");

        let encoding = venc
            .and_then(|v| v.descendants().find(|n| n.tag_name().name() == "Encoding"))
            .and_then(|n| n.text())
            .map(str::to_string);

        let resolution = venc.and_then(|v| {
            let dim = |name: &str| {
                v.descendants()
                    .find(|n| n.tag_name().name() == name)
                    .and_then(|n| n.text())
                    .and_then(|t| t.trim().parse::<u32>().ok())
            };
            dim("Width").zip(dim("Height"))
        });

        // SourceToken lives on VideoSourceConfiguration, not VideoEncoderConfiguration.
        let vsrc = prof
            .descendants()
            .find(|n| n.tag_name().name() == "VideoSourceConfiguration");

        let source_token = vsrc
            .and_then(|v| {
                v.descendants()
                    .find(|n| n.tag_name().name() == "SourceToken")
            })
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        out.push(MediaProfile {
            token,
            name,
            encoding,
            resolution,
            rtsp_uri: None,
            snapshot_uri: None,
            source_token,
        });
    }

    if out.is_empty() {
        return Err("no media profiles returned".into());
    }
    Ok(out)
}

pub(super) fn get_stream_uri(
    agent: &ureq::Agent,
    media_uri: &str,
    creds: &Credentials,
    created: &str,
    token: &str,
) -> Result<String, String> {
    let body = format!(
        concat!(
            r#"<trt:GetStreamUri><trt:StreamSetup>"#,
            r#"<tt:Stream>RTP-Unicast</tt:Stream>"#,
            r#"<tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport>"#,
            r#"</trt:StreamSetup><trt:ProfileToken>{token}</trt:ProfileToken></trt:GetStreamUri>"#
        ),
        token = xml_escape(token),
    );
    let action = format!("{NS_MEDIA}/GetStreamUri");
    let resp = soap_call(agent, media_uri, &action, creds, created, &body)?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "Uri")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no stream URI in response".to_string())
}

pub(super) fn get_snapshot_uri(
    agent: &ureq::Agent,
    media_uri: &str,
    creds: &Credentials,
    created: &str,
    token: &str,
) -> Result<String, String> {
    let body = format!(
        "<trt:GetSnapshotUri><trt:ProfileToken>{}</trt:ProfileToken></trt:GetSnapshotUri>",
        xml_escape(token)
    );
    let action = format!("{NS_MEDIA}/GetSnapshotUri");
    let resp = soap_call(agent, media_uri, &action, creds, created, &body)?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "Uri")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no snapshot URI in response".to_string())
}
