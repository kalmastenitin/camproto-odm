// src/onvif/ptz.rs

use super::soap::{build_agent, device_created, soap_call, xml_escape, NS_PTZ};
use super::types::{Credentials, PtzInfo, PtzPreset};

pub fn continuous_move(
    ptz: &PtzInfo,
    creds: &Credentials,
    pan: f32,
    tilt: f32,
    zoom: f32,
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tptz:ContinuousMove>\
         <tptz:ProfileToken>{token}</tptz:ProfileToken>\
         <tptz:Velocity>\
         <tt:PanTilt x=\"{pan}\" y=\"{tilt}\"/>\
         <tt:Zoom x=\"{zoom}\"/>\
         </tptz:Velocity>\
         </tptz:ContinuousMove>",
        token = xml_escape(&ptz.profile_token),
    );
    soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/ContinuousMove"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

/// Halt continuous motion on both pan/tilt and zoom axes.
pub fn stop(ptz: &PtzInfo, creds: &Credentials) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tptz:Stop>\
         <tptz:ProfileToken>{token}</tptz:ProfileToken>\
         <tptz:PanTilt>true</tptz:PanTilt>\
         <tptz:Zoom>true</tptz:Zoom>\
         </tptz:Stop>",
        token = xml_escape(&ptz.profile_token),
    );
    soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/Stop"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

pub fn get_presets(ptz: &PtzInfo, creds: &Credentials) -> Result<Vec<PtzPreset>, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tptz:GetPresets><tptz:ProfileToken>{}</tptz:ProfileToken></tptz:GetPresets>",
        xml_escape(&ptz.profile_token)
    );
    let resp = soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/GetPresets"),
        creds,
        &created,
        &body,
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    let mut out = Vec::new();
    for preset in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Preset")
    {
        let token = preset.attribute("token").unwrap_or_default().to_string();
        let name = preset
            .children()
            .find(|n| n.tag_name().name() == "Name")
            .and_then(|n| n.text())
            .unwrap_or_default()
            .to_string();
        if !token.is_empty() {
            out.push(PtzPreset { token, name });
        }
    }
    Ok(out)
}

/// Create or overwrite a preset at the current PTZ position. Returns the
/// preset token assigned by the device.
pub fn set_preset(ptz: &PtzInfo, creds: &Credentials, name: &str) -> Result<String, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let name_xml = if name.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<tptz:PresetName>{}</tptz:PresetName>",
            xml_escape(name.trim())
        )
    };
    let body = format!(
        "<tptz:SetPreset>\
         <tptz:ProfileToken>{}</tptz:ProfileToken>{name_xml}\
         </tptz:SetPreset>",
        xml_escape(&ptz.profile_token),
    );
    let resp = soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/SetPreset"),
        creds,
        &created,
        &body,
    )?;
    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    Ok(doc
        .descendants()
        .find(|n| n.tag_name().name() == "PresetToken")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .unwrap_or_default())
}

pub fn goto_preset(ptz: &PtzInfo, creds: &Credentials, preset_token: &str) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tptz:GotoPreset>\
         <tptz:ProfileToken>{}</tptz:ProfileToken>\
         <tptz:PresetToken>{}</tptz:PresetToken>\
         </tptz:GotoPreset>",
        xml_escape(&ptz.profile_token),
        xml_escape(preset_token),
    );
    soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/GotoPreset"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

pub fn remove_preset(ptz: &PtzInfo, creds: &Credentials, preset_token: &str) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &ptz.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<tptz:RemovePreset>\
         <tptz:ProfileToken>{}</tptz:ProfileToken>\
         <tptz:PresetToken>{}</tptz:PresetToken>\
         </tptz:RemovePreset>",
        xml_escape(&ptz.profile_token),
        xml_escape(preset_token),
    );
    soap_call(
        &agent,
        &ptz.service_uri,
        &format!("{NS_PTZ}/RemovePreset"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}
