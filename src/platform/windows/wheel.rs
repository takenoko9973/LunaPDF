pub(crate) const DEFAULT_WHEEL_SCROLL_LINES: u32 = 3;

const WHEEL_PAGESCROLL: u32 = u32::MAX;
const LOGICAL_POINTS_PER_LINE: f32 = 16.0;

pub(crate) fn effective_wheel_lines(lines: u32) -> u32 {
    if lines == 0 || lines == WHEEL_PAGESCROLL {
        DEFAULT_WHEEL_SCROLL_LINES
    } else {
        lines
    }
}

pub(crate) fn calculate_line_scroll_speed(lines: u32, percent: u16) -> f32 {
    LOGICAL_POINTS_PER_LINE * effective_wheel_lines(lines) as f32 * f32::from(percent) / 100.0
}

#[cfg(windows)]
pub(crate) fn line_scroll_speed(percent: u16) -> f32 {
    // winit 0.30.x は Windows の WM_MOUSEWHEEL を 1.0 の LineDelta として渡すだけで、
    // SPI_GETWHEELSCROLLLINES を egui のスクロール量へ反映しない。そのため LunaPDF が
    // SPI の行数から 16 論理ポイント/行を計算し、設定倍率を掛けて、100% を SumatraPDF
    // 相当の通常ホイール距離に補正する。将来 winit/eframe がこの値を反映する更新を
    // 行った場合は二重適用になるため、依存更新時にこの補正を再確認すること。
    let mut lines = 0_u32;
    let succeeded = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows_sys::Win32::UI::WindowsAndMessaging::SPI_GETWHEELSCROLLLINES,
            0,
            (&mut lines as *mut u32).cast(),
            0,
        )
    };
    let lines = if succeeded == 0 {
        DEFAULT_WHEEL_SCROLL_LINES
    } else {
        effective_wheel_lines(lines)
    };
    calculate_line_scroll_speed(lines, percent)
}
