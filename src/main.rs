// src/main.rs

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod discovery;
mod onvif;
mod video;

use app::OdmApp;
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([720.0, 460.0])
            .with_title("camproto-odm — ONVIF Device Manager"),
        ..Default::default()
    };

    eframe::run_native(
        "camproto-odm",
        options,
        Box::new(|cc| Ok(Box::new(OdmApp::new(cc)))),
    )
}
