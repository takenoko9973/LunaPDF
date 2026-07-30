#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use std::fs;

#[cfg(target_os = "windows")]
use dwrote::{FontCollection, FontStretch, FontStyle, FontWeight};
use eframe::egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use eframe::egui::{self, FontData, FontFamily};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use font_kit::family_name::FamilyName;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use font_kit::handle::Handle;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use font_kit::properties::Properties;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use font_kit::source::SystemSource;

const UI_FONT_NAME: &str = "lunapdf-ui-font";

#[cfg(target_os = "windows")]
const UI_FONT_FAMILY: &str = "Yu Gothic UI";
#[cfg(target_os = "linux")]
const UI_FONT_FAMILY: &str = "Noto Sans CJK JP";
#[cfg(target_os = "macos")]
const UI_FONT_FAMILY: &str = "Hiragino Sans";

struct LoadedUiFont {
    data: Vec<u8>,
    face_index: u32,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_exact_ui_font(
    family: &str,
    properties: Properties,
    requested_properties: Properties,
) -> bool {
    // Platform matchers may substitute a nearby family or face. Accepting either would make
    // the configured family name advisory instead of the exact cross-platform contract.
    family == UI_FONT_FAMILY && properties == requested_properties
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn load_family_font() -> Option<LoadedUiFont> {
    let properties = Properties::new();
    let source = SystemSource::new();
    let family_name = FamilyName::Title(UI_FONT_FAMILY.to_owned());
    let handle = match source.select_best_match(&[family_name], &properties) {
        Ok(handle) => handle,
        Err(error) => {
            debug_font_warning(&format!(
                "UI font family lookup failed ({UI_FONT_FAMILY}): {error}"
            ));
            return None;
        }
    };

    let font = match handle.load() {
        Ok(font) => font,
        Err(error) => {
            debug_font_warning(&format!(
                "UI font could not be loaded ({UI_FONT_FAMILY}): {error}"
            ));
            return None;
        }
    };
    if !is_exact_ui_font(&font.family_name(), font.properties(), properties) {
        debug_font_warning(&format!(
            "UI font did not provide an exact regular face: {UI_FONT_FAMILY}"
        ));
        return None;
    }

    // Convert both font-kit handle variants to the owned bytes egui requires while preserving
    // the collection face index selected by the platform source.
    let (data, face_index) = match handle {
        Handle::Path { path, font_index } => {
            let data = match fs::read(&path) {
                Ok(data) if !data.is_empty() => data,
                Ok(_) => {
                    debug_font_warning(&format!("UI font file is empty: {}", path.display()));
                    return None;
                }
                Err(error) => {
                    debug_font_warning(&format!(
                        "UI font could not be read ({}): {error}",
                        path.display()
                    ));
                    return None;
                }
            };
            (data, font_index)
        }
        Handle::Memory { bytes, font_index } => {
            if bytes.is_empty() {
                debug_font_warning("UI font memory data is empty");
                return None;
            }
            (bytes.as_ref().clone(), font_index)
        }
    };

    debug_font_family_selection(UI_FONT_FAMILY, face_index);
    Some(LoadedUiFont { data, face_index })
}

#[cfg(target_os = "windows")]
fn load_family_font() -> Option<LoadedUiFont> {
    let font_collection = FontCollection::system();
    let family = match font_collection.font_family_by_name(UI_FONT_FAMILY) {
        Ok(Some(family)) => family,
        Ok(None) => {
            debug_font_warning(&format!("UI font family was not found: {UI_FONT_FAMILY}"));
            return None;
        }
        Err(error) => {
            debug_font_warning(&format!(
                "UI font family lookup failed ({UI_FONT_FAMILY}): HRESULT {error:#010x}"
            ));
            return None;
        }
    };
    let font = match family.first_matching_font(
        FontWeight::Regular,
        FontStretch::Normal,
        FontStyle::Normal,
    ) {
        Ok(font) => font,
        Err(error) => {
            debug_font_warning(&format!(
                "UI font regular face lookup failed ({UI_FONT_FAMILY}): HRESULT {error:#010x}"
            ));
            return None;
        }
    };
    // DirectWrite may return a nearest match; silently registering it would change UI weight.
    if font.weight() != FontWeight::Regular
        || font.stretch() != FontStretch::Normal
        || font.style() != FontStyle::Normal
    {
        debug_font_warning(&format!(
            "UI font did not provide an exact regular face: {UI_FONT_FAMILY}"
        ));
        return None;
    }

    let font_face = font.create_font_face();
    let font_files = match font_face.files() {
        Ok(font_files) => font_files,
        Err(error) => {
            debug_font_warning(&format!(
                "UI font regular face files could not be queried ({UI_FONT_FAMILY}): HRESULT {error:#010x}"
            ));
            return None;
        }
    };
    // egui accepts one font blob and face index, so a face backed by multiple files is unusable.
    let [font_file] = font_files.as_slice() else {
        debug_font_warning(&format!(
            "UI font regular face did not use exactly one file: {UI_FONT_FAMILY}"
        ));
        return None;
    };
    let path = match font_file.font_file_path() {
        Ok(path) => path,
        Err(error) => {
            debug_font_warning(&format!(
                "UI font regular face path could not be queried ({UI_FONT_FAMILY}): HRESULT {error:#010x}"
            ));
            return None;
        }
    };
    let data = match fs::read(&path) {
        Ok(data) if !data.is_empty() => data,
        Ok(_) => {
            debug_font_warning(&format!("UI font file is empty: {}", path.display()));
            return None;
        }
        Err(error) => {
            debug_font_warning(&format!(
                "UI font could not be read ({}): {error}",
                path.display()
            ));
            return None;
        }
    };
    let face_index = font_face.get_index();
    debug_font_family_selection(UI_FONT_FAMILY, face_index);
    Some(LoadedUiFont { data, face_index })
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn load_family_font() -> Option<LoadedUiFont> {
    None
}

fn font_insert(data: Vec<u8>, face_index: u32) -> FontInsert {
    let mut font_data = FontData::from_owned(data);
    font_data.index = face_index;
    FontInsert::new(
        UI_FONT_NAME,
        font_data,
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Highest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    )
}

/// Installs the exact regular system UI font for proportional and monospace text.
///
/// Family resolution is platform-specific and never falls back to another family or face.
pub(crate) fn install_ui_font(ctx: &egui::Context) {
    let Some(loaded) = load_family_font() else {
        debug_font_warning("no exact system UI font was found");
        return;
    };
    ctx.add_font(font_insert(loaded.data, loaded.face_index));
}

#[cfg(debug_assertions)]
fn debug_font_family_selection(family: &str, face_index: u32) {
    eprintln!("[lunapdf] UI font family: {family}; face-index: {face_index}");
}

#[cfg(not(debug_assertions))]
fn debug_font_family_selection(_family: &str, _face_index: u32) {}

#[cfg(debug_assertions)]
fn debug_font_warning(message: &str) {
    eprintln!("[lunapdf] {message}");
}

#[cfg(not(debug_assertions))]
fn debug_font_warning(_message: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_insert_sets_face_index_and_both_ui_families() {
        let insert = font_insert(vec![1, 2, 3], 2);
        assert_eq!(insert.name, UI_FONT_NAME);
        assert_eq!(insert.data.index, 2);
        assert_eq!(insert.families.len(), 2);
        assert_eq!(insert.families[0].family, FontFamily::Proportional);
        assert_eq!(insert.families[1].family, FontFamily::Monospace);
        assert!(matches!(insert.families[0].priority, FontPriority::Highest));
        assert!(matches!(insert.families[1].priority, FontPriority::Lowest));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn exact_ui_font_match_rejects_another_family_or_face() {
        let requested = Properties::new();
        assert!(is_exact_ui_font(UI_FONT_FAMILY, requested, requested));
        assert!(!is_exact_ui_font("another-family", requested, requested));

        let mut bold = requested;
        bold.weight = font_kit::properties::Weight::BOLD;
        assert!(!is_exact_ui_font(UI_FONT_FAMILY, bold, requested));
    }
}
