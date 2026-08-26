#[cfg(any(windows, test))]
pub(crate) mod windows;

#[cfg(windows)]
pub(crate) fn line_scroll_speed(percent: u16) -> f32 {
    windows::wheel::line_scroll_speed(percent)
}

#[cfg(not(windows))]
pub(crate) fn line_scroll_speed(percent: u16) -> f32 {
    let native_line_scroll_speed = eframe::egui::InputOptions::default().line_scroll_speed;
    native_line_scroll_speed * f32::from(percent) / 100.0
}
