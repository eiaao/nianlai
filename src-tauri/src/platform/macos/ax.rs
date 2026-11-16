use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::NSWorkspace;
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_foundation::{NSRect, NSString, NSURL};
use std::ptr::{self, NonNull};

pub fn accessibility_status() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn caret_bounds(pid: i32) -> Option<NSRect> {
    autoreleasepool(|_| {
        if !accessibility_status() {
            return None;
        }
        unsafe {
            let app = AXUIElement::new_application(pid);
            let name = NSString::from_str("AXFocusedUIElement");
            let mut raw = ptr::null();
            let error = app.copy_attribute_value(name.as_ref(), NonNull::from(&mut raw));
            if error != AXError::Success {
                return None;
            }
            let focused = Retained::from_raw(raw.cast_mut())?;
            let element = focused.downcast_ref::<AXUIElement>()?;

            let name = NSString::from_str("AXSelectedTextRange");
            let mut raw = ptr::null();
            let error = element.copy_attribute_value(name.as_ref(), NonNull::from(&mut raw));
            if error != AXError::Success {
                return None;
            }
            let range = Retained::from_raw(raw.cast_mut())?;

            let name = NSString::from_str("AXBoundsForRange");
            let mut raw = ptr::null();
            let error = element.copy_parameterized_attribute_value(
                name.as_ref(),
                &range,
                NonNull::from(&mut raw),
            );
            if error != AXError::Success {
                return None;
            }
            let bounds = Retained::from_raw(raw.cast_mut())?;
            let value = bounds.downcast_ref::<AXValue>()?;
            let mut rect = NSRect::default();
            value
                .value(AXValueType::CGRect, NonNull::from(&mut rect).cast())
                .then_some(rect)
        }
    })
}

// AX 和 Tauri 都使用左上原点的点坐标，Retina 缩放由 Tauri 处理。
pub fn ax_rect_to_logical(rect: NSRect) -> [f64; 4] {
    [
        rect.origin.x,
        rect.origin.y,
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
    ]
}

pub fn open_accessibility_settings() -> Result<(), String> {
    autoreleasepool(|_| {
        let url = NSURL::URLWithString(&NSString::from_str(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        ))
        .ok_or("无法打开系统设置")?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err("无法打开系统设置".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSPoint, NSSize};

    #[test]
    fn primary_display_coordinates_are_logical_points() {
        assert_eq!(
            ax_rect_to_logical(NSRect::new(
                NSPoint::new(100.0, 200.0),
                NSSize::new(2.0, 18.0)
            )),
            [100.0, 200.0, 102.0, 218.0]
        );
    }

    #[test]
    fn left_display_keeps_negative_x() {
        assert_eq!(
            ax_rect_to_logical(NSRect::new(
                NSPoint::new(-900.0, 120.0),
                NSSize::new(2.0, 18.0)
            )),
            [-900.0, 120.0, -898.0, 138.0]
        );
    }

    #[test]
    fn upper_display_keeps_negative_y() {
        assert_eq!(
            ax_rect_to_logical(NSRect::new(
                NSPoint::new(100.0, -700.0),
                NSSize::new(2.0, 18.0)
            )),
            [100.0, -700.0, 102.0, -682.0]
        );
    }
}
