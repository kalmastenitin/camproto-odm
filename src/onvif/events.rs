// /src/onvif/events.rs

use super::soap::{build_agent, device_created, soap_call, soap_call_addressed, NS_EVENTS};
use super::types::{Credentials, EventNotification, EventsInfo};
use std::time::SystemTime;

/// A live PullPoint subscription. The `endpoint` is the URL the camera
/// returned in CreatePullPointSubscriptionResponse — subsequent PullMessages
/// and Renew calls go there, NOT to the events service URI.
#[derive(Clone)]
pub struct PullSubscription {
    pub endpoint: String,
    pub device_uri: String, // for device_created()
}

pub fn create_pull_subscription(
    events: &EventsInfo,
    creds: &Credentials,
) -> Result<PullSubscription, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &events.device_uri).ok_or_else(|| "no device clock".to_string())?;

    // InitialTerminationTime PT60S = 60 seconds. Camera will drop the
    // subscription after that if we don't Renew.
    let body = "<tev:CreatePullPointSubscription>\
                <tev:InitialTerminationTime>PT60S</tev:InitialTerminationTime>\
                </tev:CreatePullPointSubscription>";

    let resp = soap_call(
        &agent,
        &events.service_uri,
        &format!("{NS_EVENTS}/CreatePullPointSubscription"),
        creds,
        &created,
        body,
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;

    // The subscription reference is inside SubscriptionReference/Address.
    let endpoint = doc
        .descendants()
        .find(|n| n.tag_name().name() == "SubscriptionReference")
        .and_then(|sr| sr.descendants().find(|n| n.tag_name().name() == "Address"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        // Some cameras omit SubscriptionReference and just want subsequent
        // calls back to the events service URI. Fall back to that.
        .unwrap_or_else(|| events.service_uri.clone());

    Ok(PullSubscription {
        endpoint,
        device_uri: events.device_uri.clone(),
    })
}

/// Long-poll for events. The camera holds the response for up to `timeout_secs`
/// seconds waiting for events; returns immediately if the queue has any.
pub fn pull_messages(
    sub: &PullSubscription,
    creds: &Credentials,
    timeout_secs: u32,
    max_messages: u32,
) -> Result<Vec<EventNotification>, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &sub.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tev:PullMessages>\
         <tev:Timeout>PT{timeout_secs}S</tev:Timeout>\
         <tev:MessageLimit>{max_messages}</tev:MessageLimit>\
         </tev:PullMessages>"
    );

    let resp = soap_call_addressed(
        &agent,
        &sub.endpoint,
        &sub.endpoint,
        "http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest",
        creds,
        &created,
        &body,
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    let mut out = Vec::new();
    for msg in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "NotificationMessage")
    {
        // Topic
        let topic = msg
            .descendants()
            .find(|n| n.tag_name().name() == "Topic")
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        // Source / Data — the Message element contains <Source> and <Data>,
        // each with SimpleItem children carrying Name/Value attribute pairs.
        let message_node = msg.descendants().find(|n| n.tag_name().name() == "Message");

        let source = message_node
            .and_then(|m| m.descendants().find(|n| n.tag_name().name() == "Source"))
            .and_then(|src| {
                src.descendants()
                    .find(|n| n.tag_name().name() == "SimpleItem")
                    .and_then(|si| si.attribute("Value"))
                    .map(str::to_string)
            });

        let data: Vec<(String, String)> = message_node
            .and_then(|m| m.descendants().find(|n| n.tag_name().name() == "Data"))
            .map(|d| {
                d.descendants()
                    .filter(|n| n.tag_name().name() == "SimpleItem")
                    .filter_map(|si| {
                        let name = si.attribute("Name")?.to_string();
                        let value = si.attribute("Value")?.to_string();
                        Some((name, value))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Capture the raw fragment for debugging — this is helpful when a
        // vendor puts something interesting in a non-standard element.
        let raw = format!("{msg:?}");

        out.push(EventNotification {
            received_at: SystemTime::now(),
            topic,
            source,
            data,
            raw,
        });
    }

    Ok(out)
}

pub fn renew_subscription(sub: &PullSubscription, creds: &Credentials) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &sub.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = "<wsnt:Renew><wsnt:TerminationTime>PT60S</wsnt:TerminationTime></wsnt:Renew>";
    soap_call_addressed(
        &agent,
        &sub.endpoint,
        &sub.endpoint,
        "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/RenewRequest",
        creds,
        &created,
        body,
    )?;
    Ok(())
}

pub fn unsubscribe(sub: &PullSubscription, creds: &Credentials) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &sub.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = "<wsnt:Unsubscribe/>";
    soap_call_addressed(
        &agent,
        &sub.endpoint,
        &sub.endpoint,
        "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest",
        creds,
        &created,
        body,
    )?;
    Ok(())
}
