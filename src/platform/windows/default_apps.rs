use std::io;

pub(crate) const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps?registeredAppUser=LunaPDF";
#[cfg(windows)]
const PDF_EXTENSION: &str = ".pdf";
const LUNAPDF_PROG_ID: &str = "LunaPDF.Document.1";
const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
// `ERROR_NO_ASSOCIATION` を `HRESULT_FROM_WIN32` へ変換した公開APIの戻り値。
const ERROR_NO_ASSOCIATION: u32 = 1155;
const NO_ASSOCIATION_HRESULT: i32 = (0x8007_0000_u32 | ERROR_NO_ASSOCIATION) as i32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DefaultAppState {
    LunaPdf,
    Other,
    Unavailable(String),
}

/// 実際の`.pdf`関連付けで返されたProgIDだけを既定アプリ状態へ変換する。
pub(crate) fn classify_default_app(prog_id: Option<&str>) -> DefaultAppState {
    match prog_id {
        Some(prog_id) if prog_id.eq_ignore_ascii_case(LUNAPDF_PROG_ID) => DefaultAppState::LunaPdf,
        _ => DefaultAppState::Other,
    }
}

/// 既定アプリメニューの表示と操作可否を状態から決める。
pub(crate) fn default_app_menu_item(state: &DefaultAppState) -> (&'static str, bool) {
    match state {
        DefaultAppState::LunaPdf => ("既定のPDFアプリ（設定済み）", false),
        DefaultAppState::Other => ("既定のPDFアプリを設定…", true),
        DefaultAppState::Unavailable(_) => ("既定のPDFアプリを設定…（状態確認失敗）", true),
    }
}

/// `AssocQueryStringW` のサイズ照会と本照会を共通化する。
///
/// `ASSOCSTR_PROGID` は終端NULを含む必要サイズを返すため、その値をそのままUTF-16
/// バッファーの容量に使う。関連付けが存在しないという公開APIの結果だけは正常な空値と
/// して扱い、それ以外のHRESULTは呼び出し元へ返す。
fn query_string_with_buffer<F>(mut query: F) -> io::Result<Option<String>>
where
    F: FnMut(*mut u16, &mut u32) -> i32,
{
    let mut required_length = 0;
    let first_result = query(std::ptr::null_mut(), &mut required_length);
    if first_result == NO_ASSOCIATION_HRESULT {
        return Ok(None);
    }
    if first_result != S_FALSE {
        return Err(hresult_error(
            "AssocQueryStringW のサイズ照会",
            first_result,
        ));
    }
    if required_length == 0 {
        return Ok(None);
    }

    let mut buffer = vec![0_u16; required_length as usize];
    let result = query(buffer.as_mut_ptr(), &mut required_length);
    if result != S_OK {
        return Err(hresult_error("AssocQueryStringW の本照会", result));
    }

    let reported_length = (required_length as usize).min(buffer.len());
    let string_length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(reported_length);
    String::from_utf16(&buffer[..string_length])
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn hresult_error(operation: &str, result: i32) -> io::Error {
    io::Error::other(format!(
        "{operation}が失敗しました（HRESULT=0x{:08X}）",
        result as u32
    ))
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
use windows_sys::Win32::Foundation::HINSTANCE;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::{
    ASSOCF_NONE, ASSOCSTR_PROGID, AssocQueryStringW, ShellExecuteW,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

#[cfg(windows)]
fn wide_string(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
pub(crate) fn query_default_app_state() -> io::Result<DefaultAppState> {
    let extension = wide_string(PDF_EXTENSION);
    let prog_id = query_string_with_buffer(|output, length| unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_PROGID,
            extension.as_ptr(),
            std::ptr::null(),
            output,
            length,
        )
    })?;
    Ok(classify_default_app(prog_id.as_deref()))
}

#[cfg(windows)]
fn shell_execute_result(result: HINSTANCE) -> io::Result<()> {
    let result = result as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ShellExecuteW が失敗しました（戻り値={result}）"
        )))
    }
}

/// Windowsの正規の既定アプリ設定画面を、LunaPDFの登録名を指定して開く。
///
/// Settings URIのactivationだけを行い、関連付けやUserChoiceを直接変更しない。
#[cfg(windows)]
pub(crate) fn open_default_apps_settings() -> io::Result<()> {
    let uri = wide_string(DEFAULT_APPS_URI);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            uri.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    shell_execute_result(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_apps_uri_targets_the_per_user_registered_application() {
        assert_eq!(
            DEFAULT_APPS_URI,
            "ms-settings:defaultapps?registeredAppUser=LunaPDF"
        );
    }

    #[test]
    fn luna_pdf_prog_id_is_the_effective_default_association() {
        assert_eq!(
            classify_default_app(Some("LunaPDF.Document.1")),
            DefaultAppState::LunaPdf
        );
    }

    #[test]
    fn another_prog_id_is_not_luna_pdf() {
        assert_eq!(
            classify_default_app(Some("OtherPdf.Document")),
            DefaultAppState::Other
        );
    }

    #[test]
    fn missing_prog_id_is_not_luna_pdf() {
        assert_eq!(classify_default_app(None), DefaultAppState::Other);
    }

    #[test]
    fn association_query_uses_the_second_buffer_for_a_long_result() {
        let expected = format!("LunaPDF.{}", "Document".repeat(128));
        let encoded = expected.encode_utf16().collect::<Vec<_>>();
        let mut calls = 0;
        let actual = query_string_with_buffer(|output, length| {
            calls += 1;
            if output.is_null() {
                assert_eq!(*length, 0);
                *length = (encoded.len() + 1) as u32;
                S_FALSE
            } else {
                assert_eq!(*length as usize, encoded.len() + 1);
                unsafe {
                    std::ptr::copy_nonoverlapping(encoded.as_ptr(), output, encoded.len());
                    *output.add(encoded.len()) = 0;
                }
                *length = encoded.len() as u32;
                S_OK
            }
        })
        .unwrap();

        assert_eq!(calls, 2);
        assert_eq!(actual.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn association_query_treats_no_association_as_an_empty_state() {
        let result = query_string_with_buffer(|_, _| NO_ASSOCIATION_HRESULT).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn association_query_treats_an_empty_required_length_as_no_association() {
        let result = query_string_with_buffer(|_, length| {
            *length = 0;
            S_FALSE
        })
        .unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn default_app_menu_is_disabled_only_when_luna_pdf_is_default() {
        assert_eq!(
            default_app_menu_item(&DefaultAppState::LunaPdf),
            ("既定のPDFアプリ（設定済み）", false)
        );
        assert_eq!(
            default_app_menu_item(&DefaultAppState::Other),
            ("既定のPDFアプリを設定…", true)
        );
        assert_eq!(
            default_app_menu_item(&DefaultAppState::Unavailable("照会失敗".to_owned())),
            ("既定のPDFアプリを設定…（状態確認失敗）", true)
        );
    }

    #[cfg(windows)]
    #[test]
    fn shell_execute_accepts_only_return_values_above_32() {
        assert!(shell_execute_result(33 as HINSTANCE).is_ok());
        assert!(shell_execute_result(32 as HINSTANCE).is_err());
    }
}
