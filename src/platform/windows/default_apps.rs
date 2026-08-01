use std::io;
use std::process::Command;

pub(crate) const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps?registeredAppUser=LunaPDF";

/// Windows 11の既定のアプリ設定を、LunaPDFの登録名を指定して開く。
///
/// Settings URIの起動だけを行い、関連付けやUserChoiceを直接変更しない。
pub(crate) fn open_default_apps_settings() -> io::Result<()> {
    Command::new("explorer.exe")
        .arg(DEFAULT_APPS_URI)
        .spawn()
        .map(|_child| ())
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
}
