//! Fork-local title bar content-row height. Decoupled from upstream's
//! `platform_title_bar_height` so increasing our content row doesn't
//! also enlarge the platform window-controls row. Picked at 36 px to
//! comfortably fit a horizontal solution-tab strip with readable labels.

use gpui::{Pixels, px};

pub const FORK_TITLE_BAR_CONTENT_HEIGHT_PX: f32 = 36.0;

pub fn fork_title_bar_content_height() -> Pixels {
    px(FORK_TITLE_BAR_CONTENT_HEIGHT_PX)
}
