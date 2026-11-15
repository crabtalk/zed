use gpui::WindowAppearance;
use objc2_app_kit::{self as app_kit, NSAppearance};
use objc2_foundation::NSArray;

pub(crate) fn window_appearance_from_native(appearance: Option<&NSAppearance>) -> WindowAppearance {
    let Some(appearance) = appearance else {
        return WindowAppearance::Light;
    };
    // SAFETY: AppKit's exported names are immutable process-lifetime objects.
    let names = unsafe {
        [
            app_kit::NSAppearanceNameVibrantLight,
            app_kit::NSAppearanceNameVibrantDark,
            app_kit::NSAppearanceNameAqua,
            app_kit::NSAppearanceNameDarkAqua,
        ]
    };
    match appearance.bestMatchFromAppearancesWithNames(&NSArray::from_slice(&names)) {
        Some(name) if *name == *names[0] => WindowAppearance::VibrantLight,
        Some(name) if *name == *names[1] => WindowAppearance::VibrantDark,
        Some(name) if *name == *names[3] => WindowAppearance::Dark,
        _ => WindowAppearance::Light,
    }
}
