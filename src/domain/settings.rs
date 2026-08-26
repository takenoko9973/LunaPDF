use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_WHEEL_SCROLL_SPEED_PERCENT: u16 = 100;
pub(crate) const MIN_WHEEL_SCROLL_SPEED_PERCENT: u16 = 25;
pub(crate) const MAX_WHEEL_SCROLL_SPEED_PERCENT: u16 = 300;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppSettings {
    #[serde(default = "default_wheel_scroll_speed_percent")]
    pub(crate) wheel_scroll_speed: u16,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            wheel_scroll_speed: DEFAULT_WHEEL_SCROLL_SPEED_PERCENT,
        }
    }
}

impl AppSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(MIN_WHEEL_SCROLL_SPEED_PERCENT..=MAX_WHEEL_SCROLL_SPEED_PERCENT)
            .contains(&self.wheel_scroll_speed)
        {
            bail!(
                "wheel scroll speed must be between {MIN_WHEEL_SCROLL_SPEED_PERCENT}% and {MAX_WHEEL_SCROLL_SPEED_PERCENT}%"
            );
        }
        Ok(())
    }
}

fn default_wheel_scroll_speed_percent() -> u16 {
    DEFAULT_WHEEL_SCROLL_SPEED_PERCENT
}
