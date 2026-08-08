use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::ptr::null_mut;

use anyhow::{Context, Result, anyhow, ensure};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DEVMODEW, DIB_RGB_COLORS, DM_ORIENTATION,
    DMORIENT_LANDSCAPE, DMORIENT_PORTRAIT, DeleteDC, GDI_ERROR, GetDeviceCaps, HDC, HORZRES,
    PHYSICALHEIGHT, PHYSICALWIDTH, ResetDCW, SRCCOPY, StretchDIBits, VERTRES,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_NOSELECTION, PD_PAGENUMS, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE,
    PRINTDLGW, PrintDlgW,
};

use crate::domain::document::TileSpec;
use crate::pdf::mupdf_backend::MuPdfBackend;
use crate::pdf::print_layout::{PaperOrientation, PaperSize, PrintPlan, PrintableArea};

// MuPDF の RGBA 転送と GDI の DIB ビューはいずれも、ページ数やプリンター解像度に
// かかわらず 8 MiB 以内に収まる。
const PRINT_STRIP_BUDGET_BYTES: usize = 8 * 1_024 * 1_024;

pub(super) enum PrintOutcome {
    Completed,
    Cancelled,
}

/// ネイティブダイアログを表示し、メモリ上の注釈付きドキュメントをその DC に印刷する。
pub(super) fn print_document(
    backend: &mut MuPdfBackend,
    auto_rotate: bool,
) -> Result<PrintOutcome> {
    let info = backend.info()?;
    let page_count = info.page_bounds.len();
    ensure!(
        page_count <= usize::from(u16::MAX),
        "Windows print dialog cannot represent more than {} pages",
        u16::MAX
    );
    let Some(selection) = show_print_dialog(page_count)? else {
        return Ok(PrintOutcome::Cancelled);
    };

    let PrintDialogSelection {
        dc,
        devmode,
        first_page,
        last_page,
    } = selection;
    let device = PrintDevice::new(dc, devmode)?;
    let document_name = info
        .path
        .file_name()
        .unwrap_or(info.path.as_os_str())
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let document_info = DOCINFOW {
        cbSize: i32::try_from(size_of::<DOCINFOW>())?,
        lpszDocName: document_name.as_ptr(),
        ..DOCINFOW::default()
    };
    let started = unsafe { StartDocW(device.dc(), &document_info) };
    ensure!(started > 0, "Windows printer rejected StartDoc");

    let mut print_context = PrintJobContext {
        device,
        first_page,
        last_page,
        revision: info.revision,
        page_bounds: &info.page_bounds,
        auto_rotate,
    };
    let result = print_selected_pages(backend, &mut print_context);
    if let Err(error) = result {
        // StartDoc が成功した後、部分的に送信されたジョブを破棄する有効な方法は
        // AbortDoc だけである。開いたままにするとスプーラーが停止するおそれがある。
        unsafe {
            AbortDoc(print_context.device.dc());
        }
        return Err(error);
    }
    let ended = unsafe { EndDoc(print_context.device.dc()) };
    ensure!(ended > 0, "Windows printer rejected EndDoc");
    Ok(PrintOutcome::Completed)
}

/// `StartDoc` 後に保持し、ページごとに用紙方向と実capsを更新する印刷ジョブの条件。
struct PrintJobContext<'a> {
    device: PrintDevice,
    first_page: usize,
    last_page: usize,
    revision: u64,
    page_bounds: &'a [crate::domain::document::PageRect],
    auto_rotate: bool,
}

impl PrintJobContext<'_> {
    fn plan_for_page(
        &mut self,
        page_index: usize,
        bounds: crate::domain::document::PageRect,
    ) -> Result<PrintPlan> {
        let current_orientation = self.device.orientation;
        let current_area = self.device.printable_area;
        let current_paper_size = self.device.paper_size;
        if !self.auto_rotate {
            return PrintPlan::choose(
                page_index,
                bounds,
                current_orientation,
                current_area,
                current_paper_size,
                false,
                None,
            )
            .context("PDF page does not fit the printer's printable area");
        }

        let alternate_orientation = current_orientation.opposite();
        let Some((alternate_area, alternate_paper_size)) =
            self.device.try_set_orientation(alternate_orientation)?
        else {
            return PrintPlan::choose(
                page_index,
                bounds,
                current_orientation,
                self.device.printable_area,
                self.device.paper_size,
                true,
                None,
            )
            .context("PDF page does not fit the printer's printable area");
        };
        let mut plan = PrintPlan::choose(
            page_index,
            bounds,
            current_orientation,
            current_area,
            current_paper_size,
            true,
            Some((alternate_area, alternate_paper_size)),
        )
        .context("PDF page does not fit the printer's printable area")?;

        if plan.paper_orientation == current_orientation {
            // 候補比較のために一度切り替えたDCを、選択結果に合わせてStartPage前に戻す。
            // 戻せない場合は、driverが公開方向変更に対応しない状態として、現在DCの実caps
            // とPDF内容回転だけで計画を作り直す。
            if self
                .device
                .try_set_orientation(current_orientation)?
                .is_none()
            {
                return PrintPlan::choose(
                    page_index,
                    bounds,
                    self.device.orientation,
                    self.device.printable_area,
                    self.device.paper_size,
                    true,
                    None,
                )
                .context("PDF page does not fit the printer's printable area");
            }
            plan = PrintPlan::choose(
                page_index,
                bounds,
                current_orientation,
                self.device.printable_area,
                self.device.paper_size,
                true,
                Some((alternate_area, alternate_paper_size)),
            )
            .context("PDF page does not fit the printer's printable area")?;
            if plan.paper_orientation != self.device.orientation
                && self
                    .device
                    .try_set_orientation(plan.paper_orientation)?
                    .is_none()
            {
                return PrintPlan::choose(
                    page_index,
                    bounds,
                    self.device.orientation,
                    self.device.printable_area,
                    self.device.paper_size,
                    true,
                    None,
                )
                .context("PDF page does not fit the printer's printable area");
            }
        }
        Ok(plan)
    }
}

struct PrintDevice {
    dc: OwnedDc,
    devmode: OwnedDevMode,
    orientation: PaperOrientation,
    printable_area: PrintableArea,
    paper_size: PaperSize,
    orientation_change_supported: bool,
}

impl PrintDevice {
    fn new(dc: OwnedDc, devmode: OwnedDevMode) -> Result<Self> {
        let (printable_area, paper_size) = device_state(dc.0)?;
        let (orientation_change_supported, devmode_orientation) = devmode_state(devmode.0);
        let orientation =
            devmode_orientation.unwrap_or_else(|| orientation_from_area(printable_area));
        Ok(Self {
            dc,
            devmode,
            orientation,
            printable_area,
            paper_size,
            orientation_change_supported,
        })
    }

    fn dc(&self) -> HDC {
        self.dc.0
    }

    fn try_set_orientation(
        &mut self,
        orientation: PaperOrientation,
    ) -> Result<Option<(PrintableArea, PaperSize)>> {
        if orientation == self.orientation {
            return Ok(Some((self.printable_area, self.paper_size)));
        }
        if !self.orientation_change_supported || self.devmode.0.is_null() {
            return Ok(None);
        }

        let Some(devmode_ptr) = lock_devmode(self.devmode.0) else {
            self.orientation_change_supported = false;
            return Ok(None);
        };
        let old_value = unsafe { (*devmode_ptr).Anonymous1.Anonymous1.dmOrientation };
        let new_value = match orientation {
            PaperOrientation::Portrait => DMORIENT_PORTRAIT as i16,
            PaperOrientation::Landscape => DMORIENT_LANDSCAPE as i16,
        };
        unsafe {
            (*devmode_ptr).Anonymous1.Anonymous1.dmOrientation = new_value;
        }
        // 同じDEVMODEWの公開dmOrientationだけを書き換え、driver private dataには触れずに
        // ResetDCWへ渡す。hDevModeは印刷ジョブの終了まで所有し続ける。
        let reset_dc = unsafe { ResetDCW(self.dc.0, devmode_ptr.cast_const()) };
        if reset_dc.is_null() {
            unsafe {
                (*devmode_ptr).Anonymous1.Anonymous1.dmOrientation = old_value;
                GlobalUnlock(self.devmode.0);
            }
            let (printable_area, paper_size) = device_state(self.dc.0)?;
            ensure!(
                paper_size == self.paper_size,
                "printer changed the selected paper size after ResetDCW failed"
            );
            self.printable_area = printable_area;
            return Ok(None);
        }
        unsafe {
            GlobalUnlock(self.devmode.0);
        }
        self.dc.0 = reset_dc;
        let (printable_area, paper_size) = device_state(self.dc.0)?;
        ensure!(
            paper_size == self.paper_size,
            "ResetDCW changed the selected physical paper size"
        );
        self.orientation = orientation;
        self.printable_area = printable_area;
        self.paper_size = paper_size;
        Ok(Some((printable_area, paper_size)))
    }
}

fn device_state(dc: HDC) -> Result<(PrintableArea, PaperSize)> {
    let printable_area = PrintableArea {
        width: device_cap(dc, HORZRES as i32, "printable width")?,
        height: device_cap(dc, VERTRES as i32, "printable height")?,
    };
    let paper_size = PaperSize::from_physical_dimensions(
        device_cap(dc, PHYSICALWIDTH as i32, "physical paper width")?,
        device_cap(dc, PHYSICALHEIGHT as i32, "physical paper height")?,
    );
    Ok((printable_area, paper_size))
}

fn orientation_from_area(area: PrintableArea) -> PaperOrientation {
    if area.width > area.height {
        PaperOrientation::Landscape
    } else {
        PaperOrientation::Portrait
    }
}

fn devmode_state(handle: HGLOBAL) -> (bool, Option<PaperOrientation>) {
    let Some(devmode_ptr) = lock_devmode(handle) else {
        return (false, None);
    };
    let state = unsafe {
        let devmode = &*devmode_ptr;
        let supported = devmode.dmFields & DM_ORIENTATION != 0;
        let orientation = if supported {
            match devmode.Anonymous1.Anonymous1.dmOrientation as u32 {
                DMORIENT_PORTRAIT => Some(PaperOrientation::Portrait),
                DMORIENT_LANDSCAPE => Some(PaperOrientation::Landscape),
                _ => None,
            }
        } else {
            None
        };
        (supported && orientation.is_some(), orientation)
    };
    unsafe {
        GlobalUnlock(handle);
    }
    state
}

fn lock_devmode(handle: HGLOBAL) -> Option<*mut DEVMODEW> {
    if handle.is_null() {
        return None;
    }
    let pointer = unsafe { GlobalLock(handle) };
    (!pointer.is_null()).then_some(pointer.cast::<DEVMODEW>())
}

fn print_selected_pages(
    backend: &mut MuPdfBackend,
    context: &mut PrintJobContext<'_>,
) -> Result<()> {
    for page_index in context.first_page..=context.last_page {
        let bounds = context.page_bounds[page_index];
        let plan = context.plan_for_page(page_index, bounds)?;
        ensure!(
            plan.paper_size == context.device.paper_size,
            "print plan paper size does not match the active printer DC"
        );
        let strips = plan
            .layout
            .strips(PRINT_STRIP_BUDGET_BYTES)
            .context("printer scanline exceeds the print memory budget")?;
        let started = unsafe { StartPage(context.device.dc()) };
        ensure!(started > 0, "Windows printer rejected StartPage");

        for strip in strips {
            let spec = TileSpec {
                pixel_x: 0,
                pixel_y: strip.pixel_y,
                pixel_width: plan.layout.pixel_width,
                pixel_height: strip.pixel_height,
            };
            let mut tile = backend
                .render_print_strip(
                    page_index,
                    plan.layout.scale,
                    plan.pdf_rotation,
                    spec,
                    context.revision,
                )?
                .context("print raster became stale while the document was locked")?;
            ensure!(
                tile.spec == spec
                    && tile.page_pixel_width == plan.layout.pixel_width
                    && tile.page_pixel_height == plan.layout.pixel_height,
                "MuPDF returned unexpected print strip geometry"
            );
            rgba_to_bgra_in_place(&mut tile.pixels_rgba);

            let bitmap_info = bitmap_info(plan.layout.pixel_width, strip.pixel_height)?;
            let destination_y = plan
                .layout
                .offset_y
                .checked_add(i32::try_from(strip.pixel_y)?)
                .context("print destination exceeds GDI coordinates")?;
            let copied = unsafe {
                StretchDIBits(
                    context.device.dc(),
                    plan.layout.offset_x,
                    destination_y,
                    i32::try_from(plan.layout.pixel_width)?,
                    i32::try_from(strip.pixel_height)?,
                    0,
                    0,
                    i32::try_from(plan.layout.pixel_width)?,
                    i32::try_from(strip.pixel_height)?,
                    tile.pixels_rgba.as_ptr().cast::<c_void>(),
                    &bitmap_info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            // GDI はプリンタードライバーの失敗経路によって 0 または GDI_ERROR を返す
            // ため、どちらの値でも帯を拒否しなければならない。
            ensure!(
                copied != 0 && copied != GDI_ERROR as i32,
                "Windows printer rejected a page bitmap strip"
            );
        }
        let ended = unsafe { EndPage(context.device.dc()) };
        ensure!(ended > 0, "Windows printer rejected EndPage");
    }
    Ok(())
}

fn bitmap_info(pixel_width: u32, pixel_height: u32) -> Result<BITMAPINFO> {
    let image_bytes = pixel_width
        .checked_mul(pixel_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("print bitmap byte count overflowed")?;
    Ok(BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>())?,
            biWidth: i32::try_from(pixel_width)?,
            // DIB の高さを負にすると、MuPDF の行が上から下の順であることを GDI に伝えられる。
            biHeight: -i32::try_from(pixel_height)?,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: image_bytes,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    })
}

fn rgba_to_bgra_in_place(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

struct PrintDialogSelection {
    dc: OwnedDc,
    devmode: OwnedDevMode,
    first_page: usize,
    last_page: usize,
}

fn show_print_dialog(page_count: usize) -> Result<Option<PrintDialogSelection>> {
    let maximum_page = u16::try_from(page_count)?;
    let mut dialog = PRINTDLGW {
        lStructSize: u32::try_from(size_of::<PRINTDLGW>())?,
        hwndOwner: null_mut(),
        Flags: PD_RETURNDC | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE,
        nFromPage: 1,
        nToPage: maximum_page,
        nMinPage: 1,
        nMaxPage: maximum_page,
        nCopies: 1,
        ..PRINTDLGW::default()
    };
    let accepted = unsafe { PrintDlgW(&mut dialog) };
    if accepted == 0 {
        free_global(dialog.hDevMode);
        free_global(dialog.hDevNames);
        let error = unsafe { CommDlgExtendedError() };
        if error == 0 {
            return Ok(None);
        }
        return Err(anyhow!(
            "Windows print dialog failed with code 0x{error:08X}"
        ));
    }
    let devmode = OwnedDevMode(dialog.hDevMode);
    free_global(dialog.hDevNames);
    ensure!(
        !dialog.hDC.is_null(),
        "Windows print dialog returned no printer DC"
    );
    let dc = OwnedDc(dialog.hDC);
    let (first_page, last_page) =
        selected_page_range(dialog.Flags, dialog.nFromPage, dialog.nToPage, page_count)?;
    Ok(Some(PrintDialogSelection {
        dc,
        devmode,
        first_page,
        last_page,
    }))
}

fn selected_page_range(
    flags: u32,
    from_page: u16,
    to_page: u16,
    page_count: usize,
) -> Result<(usize, usize)> {
    if flags & PD_PAGENUMS == 0 {
        return Ok((0, page_count - 1));
    }
    let first = usize::from(from_page);
    let last = usize::from(to_page);
    // ネイティブダイアログの結果は外部境界として扱う。無効な範囲を範囲外の
    // MuPDF ページ要求へ変換してはならない。
    ensure!(
        first >= 1 && first <= last && last <= page_count,
        "Windows print dialog returned an invalid page range"
    );
    Ok((first - 1, last - 1))
}

fn device_cap(dc: HDC, index: i32, name: &str) -> Result<u32> {
    let value = unsafe { GetDeviceCaps(dc, index) };
    let value =
        u32::try_from(value).with_context(|| format!("printer returned an invalid {name}"))?;
    ensure!(value > 0, "printer returned an empty {name}");
    Ok(value)
}

fn free_global(handle: HGLOBAL) {
    if !handle.is_null() {
        unsafe {
            GlobalFree(handle);
        }
    }
}

struct OwnedDc(HDC);

struct OwnedDevMode(HGLOBAL);

impl Drop for OwnedDevMode {
    fn drop(&mut self) {
        free_global(self.0);
    }
}

impl Drop for OwnedDc {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}
