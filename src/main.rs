#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod domain;
mod pdf;
mod persistence;
mod platform;
mod render;
mod ui;

use std::path::PathBuf;
#[cfg(windows)]
use std::sync::{Arc, OnceLock};

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use crossbeam_channel::unbounded;
use eframe::egui;

use crate::app::PrototypeApp;
use crate::persistence::session_store::SessionStore;
#[cfg(windows)]
use crate::platform::windows::single_instance::SingleInstanceListener;

/// コマンドラインで渡された0個以上のPDFパスを指定してLunaPDFを起動する。
///
/// ウィンドウへPDFファイルをドロップして追加の文書を開くこともできる。Windowsでは
/// 既存プロセスへ起動引数を転送し、主プロセスだけがネイティブウィンドウを生成する。
fn main() -> Result<()> {
    let pdf_paths = pdf_paths_from_args();
    let (external_event_sender, external_event_receiver) = unbounded();
    #[cfg(windows)]
    let repaint_context = {
        let listener = match SingleInstanceListener::acquire_or_forward(&pdf_paths)? {
            Some(listener) => listener,
            None => return Ok(()),
        };
        let repaint_context = Arc::new(OnceLock::<egui::Context>::new());
        let listener_repaint_context = Arc::clone(&repaint_context);
        listener
            .spawn(move |event| {
                if external_event_sender.send(event).is_ok()
                    && let Some(context) = listener_repaint_context.get()
                {
                    context.request_repaint();
                }
            })
            .context("start LunaPDF single-instance listener")?;
        repaint_context
    };
    #[cfg(not(windows))]
    drop(external_event_sender);

    let session_store = SessionStore::for_current_user()?;
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1_200.0, 900.0])
        .with_min_inner_size([720.0, 540.0]);
    #[cfg(windows)]
    let viewport = viewport.with_icon(
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/windows/lunapdf-icon.png"))
            .context("decode embedded LunaPDF window icon")?,
    );
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "LunaPDF",
        native_options,
        Box::new(move |creation_context| {
            #[cfg(windows)]
            let _ = repaint_context.set(creation_context.egui_ctx.clone());
            Ok(Box::new(PrototypeApp::new(
                creation_context,
                pdf_paths,
                session_store,
                external_event_receiver,
            )))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

/// OSの文字列表現を保持したまま、実行ファイル名を除く起動引数をPDF候補として返す。
fn pdf_paths_from_args() -> Vec<PathBuf> {
    std::env::args_os().skip(1).map(PathBuf::from).collect()
}
