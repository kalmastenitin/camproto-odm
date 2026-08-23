// /src/onvif/replay.rs

use super::soap::{build_agent, device_created, soap_call, xml_escape, NS_REPLAY, NS_SEARCH};
use super::types::{Credentials, Recording, ReplayInfo};

pub fn get_recordings(replay: &ReplayInfo, creds: &Credentials) -> Result<Vec<Recording>, String> {
    eprintln!(
        "DEBUG get_recordings ENTER search_uri={} device_uri={}",
        replay.search_uri, replay.device_uri
    );
    let agent = build_agent();
    let created =
        device_created(&agent, &replay.device_uri).ok_or_else(|| "no device clock".to_string())?;
    eprintln!("DEBUG get_recordings got device clock: {created}");
    // Try the one-shot GetRecordings first — many devices (Hikvision included)
    // implement it and it's dramatically faster than the FindRecordings flow.
    // If it 404s or returns a fault, fall through to the spec-mandated search.
    if let Ok(resp) = soap_call(
        &agent,
        &replay.search_uri,
        &format!("{NS_SEARCH}/GetRecordings"),
        creds,
        &created,
        "<tse:GetRecordings/>",
    ) {
        eprintln!("DEBUG GetRecordings raw response:\n{resp}\n");
        let recs = parse_recordings(&resp);
        eprintln!("DEBUG GetRecordings parsed {} recordings", recs.len());
        if !recs.is_empty() {
            return Ok(recs);
        }
    }

    eprintln!("DEBUG entering FindRecordings fallback");

    // Full FindRecordings → poll → GetRecordingSearchResults → EndSearch flow.
    let find_body = "<tse:FindRecordings>\
                                <tse:Scope/>\
                                <tse:KeepAliveTime>PT30S</tse:KeepAliveTime>\
                            </tse:FindRecordings>";

    let find_resp = soap_call(
        &agent,
        &replay.search_uri,
        &format!("{NS_SEARCH}/FindRecordings"),
        creds,
        &created,
        find_body,
    )?;

    let search_token = roxmltree::Document::parse(&find_resp)
        .map_err(|e| format!("bad FindRecordings XML: {e}"))?
        .descendants()
        .find(|n| n.tag_name().name() == "SearchToken")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no SearchToken in FindRecordings response".to_string())?;

    let mut recordings = Vec::<Recording>::new();

    for _ in 0..30 {
        let poll_body = format!(
            "<tse:GetRecordingSearchResults>\
            <tse:SearchToken>{}</tse:SearchToken>\
            <tse:MinResults>1</tse:MinResults>\
            <tse:MaxResults>100</tse:MaxResults>\
            <tse:WaitTime>PT1S</tse:WaitTime>\
        </tse:GetRecordingSearchResults>",
            xml_escape(&search_token)
        );

        let poll_resp = soap_call(
            &agent,
            &replay.search_uri,
            &format!("{NS_SEARCH}/GetRecordingSearchResults"),
            creds,
            &created,
            &poll_body,
        )?;

        eprintln!("DEBUG GetRecordingSearchResults:\n{poll_resp}");

        // IMPORTANT: append each batch instead of replacing the previous batch.
        for rec in parse_recordings(&poll_resp) {
            if !recordings
                .iter()
                .any(|existing| existing.token == rec.token)
            {
                recordings.push(rec);
            }
        }

        let state = roxmltree::Document::parse(&poll_resp)
            .ok()
            .and_then(|doc| {
                doc.descendants()
                    .find(|n| n.tag_name().name() == "SearchState")
                    .and_then(|n| n.text())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default();

        eprintln!(
            "DEBUG recording search state={state:?}, accumulated={}",
            recordings.len()
        );

        // Do NOT stop just because the first result arrived.
        if state.eq_ignore_ascii_case("Completed") {
            break;
        }
    }

    let _ = soap_call(
        &agent,
        &replay.search_uri,
        &format!("{NS_SEARCH}/EndSearch"),
        creds,
        &created,
        &format!(
            "<tse:EndSearch>\
            <tse:SearchToken>{}</tse:SearchToken>\
        </tse:EndSearch>",
            xml_escape(&search_token)
        ),
    );

    Ok(recordings)
}

fn parse_recordings(xml: &str) -> Vec<Recording> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| {
        matches!(
            n.tag_name().name(),
            "RecordingInformation" | "RecordingItem" | "Recording"
        )
    }) {
        let token = node
            .descendants()
            .find(|n| n.tag_name().name() == "RecordingToken")
            .and_then(|n| n.text())
            .or_else(|| node.attribute("token"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if token.is_empty() {
            continue;
        }
        let earliest = node
            .descendants()
            .find(|n| n.tag_name().name() == "EarliestRecording")
            .or_else(|| node.descendants().find(|n| n.tag_name().name() == "From"))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string());
        let latest = node
            .descendants()
            .find(|n| n.tag_name().name() == "LatestRecording")
            .or_else(|| node.descendants().find(|n| n.tag_name().name() == "Until"))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string());
        let source_name = node
            .descendants()
            .find(|n| n.tag_name().name() == "Name")
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string());

        // Track sources — walk Tracks/Track/SourceToken. Some NVRs use
        // <SourceToken> inside <Track>; others use <SourceId>. Look for both.
        let track_sources: Vec<String> = node
            .descendants()
            .filter(|n| matches!(n.tag_name().name(), "SourceToken" | "SourceId"))
            .filter_map(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        out.push(Recording {
            token,
            earliest,
            latest,
            source_name,
            track_sources,
        });
    }
    out
}

/// Get the RTSP URL to replay a specific recording. The camera returns a
/// full rtsp://... URI; we then hand this to camproto-ingest with playback
/// range headers.
pub fn get_replay_uri(
    replay: &ReplayInfo,
    creds: &Credentials,
    recording_token: &str,
) -> Result<String, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &replay.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<trp:GetReplayUri>\
         <trp:StreamSetup>\
         <tt:Stream>RTP-Unicast</tt:Stream>\
         <tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport>\
         </trp:StreamSetup>\
         <trp:RecordingToken>{}</trp:RecordingToken>\
         </trp:GetReplayUri>",
        xml_escape(recording_token)
    );

    let resp = soap_call(
        &agent,
        &replay.replay_uri,
        &format!("{NS_REPLAY}/GetReplayUri"),
        creds,
        &created,
        &body,
    )?;
    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "Uri")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no Uri in GetReplayUri response".to_string())
}
