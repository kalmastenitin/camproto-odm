// /src/onvif/imaging.rs

use super::soap::{build_agent, device_created, soap_call, xml_escape, NS_IMAGING};
use super::types::{Credentials, ImagingInfo, ImagingRanges, ImagingSettings};

pub fn get_imaging_settings(
    imaging: &ImagingInfo,
    creds: &Credentials,
) -> Result<ImagingSettings, String> {
    let agent = build_agent();

    let created =
        device_created(&agent, &imaging.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<timg:GetImagingSettings><timg:VideoSourceToken>{}</timg:VideoSourceToken></timg:GetImagingSettings>",
        xml_escape(&imaging.source_token)
    );

    let resp = soap_call(
        &agent,
        &imaging.service_uri,
        &format!("{NS_IMAGING}/GetImagingSettings"),
        creds,
        &created,
        &body,
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;

    let find_f = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())
    };

    let find_s = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let child_f = |parent: &str, child: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == parent)
            .and_then(|p| p.descendants().find(|n| n.tag_name().name() == child))
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())
    };
    let child_s = |parent: &str, child: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == parent)
            .and_then(|p| p.descendants().find(|n| n.tag_name().name() == child))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    Ok(ImagingSettings {
        brightness: find_f("Brightness"),
        contrast: find_f("Contrast"),
        color_saturation: find_f("ColorSaturation"),
        sharpness: find_f("Sharpness"),

        exposure_mode: child_s("Exposure", "Mode"),
        exposure_time: child_f("Exposure", "ExposureTime"),
        exposure_gain: child_f("Exposure", "Gain"),
        exposure_iris: child_f("Exposure", "Iris"),

        white_balance_mode: child_s("WhiteBalance", "Mode"),
        white_balance_cr_gain: child_f("WhiteBalance", "CrGain"),
        white_balance_cb_gain: child_f("WhiteBalance", "CbGain"),

        wdr_mode: child_s("WideDynamicRange", "Mode"),
        wdr_level: child_f("WideDynamicRange", "Level"),

        backlight_mode: child_s("BacklightCompensation", "Mode"),
        backlight_level: child_f("BacklightCompensation", "Level"),

        image_stab_mode: child_s("ImageStabilization", "Mode"),
        image_stab_level: child_f("ImageStabilization", "Level"),

        ir_cut_filter: find_s("IrCutFilter"),

        focus_mode: child_s("Focus", "AutoFocusMode"),
    })
}

pub fn get_imaging_options(
    imaging: &ImagingInfo,
    creds: &Credentials,
) -> Result<ImagingRanges, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &imaging.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<timg:GetOptions><timg:VideoSourceToken>{}</timg:VideoSourceToken></timg:GetOptions>",
        xml_escape(&imaging.source_token)
    );
    let resp = soap_call(
        &agent,
        &imaging.service_uri,
        &format!("{NS_IMAGING}/GetOptions"),
        creds,
        &created,
        &body,
    )?;
    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;

    let range = |name: &str| -> Option<(f32, f32)> {
        let node = doc.descendants().find(|n| n.tag_name().name() == name)?;
        let min = node
            .descendants()
            .find(|n| n.tag_name().name() == "Min")
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())?;
        let max = node
            .descendants()
            .find(|n| n.tag_name().name() == "Max")
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())?;
        Some((min, max))
    };

    let child_range = |parent: &str, child: &str| -> Option<(f32, f32)> {
        let p = doc.descendants().find(|n| n.tag_name().name() == parent)?;
        let node = p.descendants().find(|n| n.tag_name().name() == child)?;
        let min = node
            .descendants()
            .find(|n| n.tag_name().name() == "Min")
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())?;
        let max = node
            .descendants()
            .find(|n| n.tag_name().name() == "Max")
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<f32>().ok())?;
        Some((min, max))
    };

    // Enumerated modes come back as multiple "Mode" child elements under the
    // relevant parent. Collect them all.
    let modes = |parent: &str| -> Vec<String> {
        doc.descendants()
            .find(|n| n.tag_name().name() == parent)
            .map(|p| {
                p.descendants()
                    .filter(|n| n.tag_name().name() == "Mode")
                    .filter_map(|n| n.text())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    Ok(ImagingRanges {
        brightness: range("Brightness"),
        contrast: range("Contrast"),
        color_saturation: range("ColorSaturation"),
        sharpness: range("Sharpness"),
        exposure_time: child_range("Exposure", "ExposureTime"),
        exposure_gain: child_range("Exposure", "Gain"),
        exposure_iris: child_range("Exposure", "Iris"),
        wb_cr_gain: child_range("WhiteBalance", "YrGain"),
        wb_cb_gain: child_range("WhiteBalance", "YbGain"),
        wdr_level: child_range("WideDynamicRange", "Level"),
        backlight_level: child_range("BacklightCompensation", "Level"),
        image_stab_level: child_range("ImageStabilization", "Level"),
        focus_speed: child_range("Focus", "DefaultSpeed"),
        exposure_modes: modes("Exposure"),
        wb_modes: modes("WhiteBalance"),
        wdr_modes: modes("WideDynamicRange"),
        backlight_modes: modes("BacklightCompensation"),
        image_stab_modes: modes("ImageStabilization"),
        ir_cut_modes: doc
            .descendants()
            .filter(|n| n.tag_name().name() == "IrCutFilterModes")
            .filter_map(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        focus_modes: {
            doc.descendants()
                .find(|n| n.tag_name().name() == "Focus")
                .map(|p| {
                    p.descendants()
                        .filter(|n| n.tag_name().name() == "AutoFocusModes")
                        .filter_map(|n| n.text())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        },
    })
}

pub fn set_imaging_settings(
    imaging: &ImagingInfo,
    creds: &Credentials,
    s: &ImagingSettings,
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &imaging.device_uri).ok_or_else(|| "no device clock".to_string())?;

    // Build the ImagingSettings body incrementally — only include fields the
    // caller actually set (Some). Empty Some values on a camera that doesn't
    // support that field get rejected as "Argument Value Invalid".
    let mut inner = String::new();

    let push_f = |dst: &mut String, tag: &str, val: Option<f32>| {
        if let Some(v) = val {
            dst.push_str(&format!("<tt:{tag}>{v}</tt:{tag}>"));
        }
    };

    push_f(&mut inner, "Brightness", s.brightness);
    push_f(&mut inner, "ColorSaturation", s.color_saturation);
    push_f(&mut inner, "Contrast", s.contrast);
    push_f(&mut inner, "Sharpness", s.sharpness);

    if s.exposure_mode.is_some()
        || s.exposure_time.is_some()
        || s.exposure_gain.is_some()
        || s.exposure_iris.is_some()
    {
        inner.push_str("<tt:Exposure>");
        if let Some(m) = &s.exposure_mode {
            inner.push_str(&format!("<tt:Mode>{}</tt:Mode>", xml_escape(m)));
        }
        push_f(&mut inner, "ExposureTime", s.exposure_time);
        push_f(&mut inner, "Gain", s.exposure_gain);
        push_f(&mut inner, "Iris", s.exposure_iris);
        inner.push_str("</tt:Exposure>");
    }

    if s.focus_mode.is_some() {
        if let Some(m) = &s.focus_mode {
            inner.push_str(&format!(
                "<tt:Focus><tt:AutoFocusMode>{}</tt:AutoFocusMode></tt:Focus>",
                xml_escape(m)
            ));
        }
    }

    if let Some(m) = &s.ir_cut_filter {
        inner.push_str(&format!(
            "<tt:IrCutFilter>{}</tt:IrCutFilter>",
            xml_escape(m)
        ));
    }

    if s.white_balance_mode.is_some()
        || s.white_balance_cr_gain.is_some()
        || s.white_balance_cb_gain.is_some()
    {
        inner.push_str("<tt:WhiteBalance>");
        if let Some(m) = &s.white_balance_mode {
            inner.push_str(&format!("<tt:Mode>{}</tt:Mode>", xml_escape(m)));
        }
        push_f(&mut inner, "CrGain", s.white_balance_cr_gain);
        push_f(&mut inner, "CbGain", s.white_balance_cb_gain);
        inner.push_str("</tt:WhiteBalance>");
    }

    if s.backlight_mode.is_some() || s.backlight_level.is_some() {
        inner.push_str("<tt:BacklightCompensation>");
        if let Some(m) = &s.backlight_mode {
            inner.push_str(&format!("<tt:Mode>{}</tt:Mode>", xml_escape(m)));
        }
        push_f(&mut inner, "Level", s.backlight_level);
        inner.push_str("</tt:BacklightCompensation>");
    }

    if s.wdr_mode.is_some() || s.wdr_level.is_some() {
        inner.push_str("<tt:WideDynamicRange>");
        if let Some(m) = &s.wdr_mode {
            inner.push_str(&format!("<tt:Mode>{}</tt:Mode>", xml_escape(m)));
        }
        push_f(&mut inner, "Level", s.wdr_level);
        inner.push_str("</tt:WideDynamicRange>");
    }

    if s.image_stab_mode.is_some() || s.image_stab_level.is_some() {
        inner.push_str("<tt:ImageStabilization>");
        if let Some(m) = &s.image_stab_mode {
            inner.push_str(&format!("<tt:Mode>{}</tt:Mode>", xml_escape(m)));
        }
        push_f(&mut inner, "Level", s.image_stab_level);
        inner.push_str("</tt:ImageStabilization>");
    }

    let body = format!(
        "<timg:SetImagingSettings>\
         <timg:VideoSourceToken>{}</timg:VideoSourceToken>\
         <timg:ImagingSettings>{inner}</timg:ImagingSettings>\
         </timg:SetImagingSettings>",
        xml_escape(&imaging.source_token),
    );

    soap_call(
        &agent,
        &imaging.service_uri,
        &format!("{NS_IMAGING}/SetImagingSettings"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

/// Focus press-and-hold: continuous. Positive = far, negative = near.
pub fn focus_continuous_move(
    imaging: &ImagingInfo,
    creds: &Credentials,
    speed: f32,
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &imaging.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<timg:Move>\
         <timg:VideoSourceToken>{}</timg:VideoSourceToken>\
         <timg:Focus><tt:Continuous><tt:Speed>{speed}</tt:Speed></tt:Continuous></timg:Focus>\
         </timg:Move>",
        xml_escape(&imaging.source_token),
    );
    soap_call(
        &agent,
        &imaging.service_uri,
        &format!("{NS_IMAGING}/Move"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

pub fn focus_stop(imaging: &ImagingInfo, creds: &Credentials) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, &imaging.device_uri).ok_or_else(|| "no device clock".to_string())?;

    let body = format!(
        "<timg:Stop><timg:VideoSourceToken>{}</timg:VideoSourceToken></timg:Stop>",
        xml_escape(&imaging.source_token)
    );
    soap_call(
        &agent,
        &imaging.service_uri,
        &format!("{NS_IMAGING}/Stop"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}
