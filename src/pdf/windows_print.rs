use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::ptr::null_mut;

use anyhow::{Context, Result, anyhow, ensure};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteDC, GDI_ERROR, GetDeviceCaps, HDC,
    HORZRES, SRCCOPY, StretchDIBits, VERTRES,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_NOSELECTION, PD_PAGENUMS, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE,
    PRINTDLGW, PrintDlgW,
};

use crate::domain::document::{RenderPriority, TileRequest, TileSpec};
use crate::pdf::mupdf_backend::MuPdfBackend;
use crate::pdf::print_layout::PrintLayout;

// MuPDF の RGBA 転送と GDI の DIB ビューはいずれも、ページ数やプリンター解像度に
// かかわらず 8 MiB 以内に収まる。
const PRINT_STRIP_BUDGET_BYTES: usize = 8 * 1_024 * 1_024;

pub(super) enum PrintOutcome {
    Completed,
    Cancelled,
}

/// ネイティブダイアログを表示し、メモリ上の注釈付きドキュメントをその DC に印刷する。
pub(super) fn print_document(backend: &mut MuPdfBackend) -> Result<PrintOutcome> {
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

    let printable_width = device_cap(selection.dc.0, HORZRES as i32, "printable width")?;
    let printable_height = device_cap(selection.dc.0, VERTRES as i32, "printable height")?;
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
    let started = unsafe { StartDocW(selection.dc.0, &document_info) };
    ensure!(started > 0, "Windows printer rejected StartDoc");

    let result = print_selected_pages(
        backend,
        selection.dc.0,
        selection.first_page,
        selection.last_page,
        printable_width,
        printable_height,
        info.revision,
        &info.page_bounds,
    );
    if let Err(error) = result {
        // StartDoc が成功した後、部分的に送信されたジョブを破棄する有効な方法は
        // AbortDoc だけである。開いたままにするとスプーラーが停止するおそれがある。
        unsafe {
            AbortDoc(selection.dc.0);
        }
        return Err(error);
    }
    let ended = unsafe { EndDoc(selection.dc.0) };
    ensure!(ended > 0, "Windows printer rejected EndDoc");
    Ok(PrintOutcome::Completed)
}

#[allow(clippy::too_many_arguments)]
fn print_selected_pages(
    backend: &mut MuPdfBackend,
    dc: HDC,
    first_page: usize,
    last_page: usize,
    printable_width: u32,
    printable_height: u32,
    revision: u64,
    page_bounds: &[crate::domain::document::PageRect],
) -> Result<()> {
    for page_index in first_page..=last_page {
        let bounds = page_bounds[page_index];
        let layout = PrintLayout::fit(bounds, printable_width, printable_height)
            .context("PDF page does not fit the printer's printable area")?;
        let strips = layout
            .strips(PRINT_STRIP_BUDGET_BYTES)
            .context("printer scanline exceeds the print memory budget")?;
        let started = unsafe { StartPage(dc) };
        ensure!(started > 0, "Windows printer rejected StartPage");

        for strip in strips {
            let request = TileRequest {
                page_index,
                zoom: layout.scale,
                pixels_per_point: 1.0,
                scale: layout.scale,
                generation: 0,
                expected_revision: revision,
                spec: TileSpec {
                    pixel_x: 0,
                    pixel_y: strip.pixel_y,
                    pixel_width: layout.pixel_width,
                    pixel_height: strip.pixel_height,
                },
                priority: RenderPriority::Visible,
            };
            let mut tile = backend
                .render_tile(request)?
                .context("print raster became stale while the document was locked")?;
            ensure!(
                tile.spec == request.spec
                    && tile.page_pixel_width == layout.pixel_width
                    && tile.page_pixel_height == layout.pixel_height,
                "MuPDF returned unexpected print strip geometry"
            );
            rgba_to_bgra_in_place(&mut tile.pixels_rgba);

            let bitmap_info = bitmap_info(layout.pixel_width, strip.pixel_height)?;
            let destination_y = layout
                .offset_y
                .checked_add(i32::try_from(strip.pixel_y)?)
                .context("print destination exceeds GDI coordinates")?;
            let copied = unsafe {
                StretchDIBits(
                    dc,
                    layout.offset_x,
                    destination_y,
                    i32::try_from(layout.pixel_width)?,
                    i32::try_from(strip.pixel_height)?,
                    0,
                    0,
                    i32::try_from(layout.pixel_width)?,
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
        let ended = unsafe { EndPage(dc) };
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
    free_global(dialog.hDevMode);
    free_global(dialog.hDevNames);
    if accepted == 0 {
        let error = unsafe { CommDlgExtendedError() };
        if error == 0 {
            return Ok(None);
        }
        return Err(anyhow!(
            "Windows print dialog failed with code 0x{error:08X}"
        ));
    }
    ensure!(
        !dialog.hDC.is_null(),
        "Windows print dialog returned no printer DC"
    );
    let dc = OwnedDc(dialog.hDC);
    let (first_page, last_page) =
        selected_page_range(dialog.Flags, dialog.nFromPage, dialog.nToPage, page_count)?;
    Ok(Some(PrintDialogSelection {
        dc,
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

impl Drop for OwnedDc {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}
