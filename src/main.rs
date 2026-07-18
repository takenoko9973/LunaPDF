mod app;
mod domain;
mod pdf;
mod render;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use eframe::egui;

use crate::app::PrototypeApp;

/// Starts LunaPDF with zero or more PDF paths supplied on the command line.
///
/// Additional documents can be opened by dropping PDF files onto the window.
fn main() -> Result<()> {
    let pdf_paths = pdf_paths_from_args();
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_200.0, 900.0])
            .with_min_inner_size([720.0, 540.0]),
        ..Default::default()
    };

    eframe::run_native(
        "LunaPDF",
        native_options,
        Box::new(move |creation_context| {
            Ok(Box::new(PrototypeApp::new(creation_context, pdf_paths)))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn pdf_paths_from_args() -> Vec<PathBuf> {
    std::env::args_os().skip(1).map(PathBuf::from).collect()
}
