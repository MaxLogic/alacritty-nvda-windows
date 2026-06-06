//! Windows UI Automation RangeFromPoint tests.

#[cfg(test)]
mod tests {
    use windows_sys::core::BSTR;
    use windows_sys::Win32::Foundation::{SysFreeString, SysStringLen};
    use windows_sys::Win32::UI::Accessibility::{TextUnit_Line, UiaPoint};

    use crate::accessibility::text_pattern::RawTextProvider;

    unsafe fn range_text(range: *mut std::ffi::c_void) -> String {
        let vtable = unsafe {
            *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable)
        };
        let mut text: BSTR = std::ptr::null_mut();
        assert_eq!(unsafe { (vtable.expand_to_enclosing_unit)(range, TextUnit_Line) }, 0);
        assert_eq!(unsafe { (vtable.get_text)(range, -1, &mut text) }, 0);
        let value = String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, SysStringLen(text) as usize)
        });
        unsafe { SysFreeString(text) };
        value
    }

    unsafe fn release_range(range: *mut std::ffi::c_void) {
        let vtable = unsafe {
            *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable)
        };
        unsafe { (vtable.release)(range) };
    }

    #[test]
    fn range_from_point_returns_line_under_grid_coordinate() {
        let provider = RawTextProvider::allocate("first\nsecond\nthird".to_owned());
        unsafe {
            (*provider.as_ptr()).set_test_layout(10.0, 20.0, 8.0, 16.0, 6, 3);
        }

        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut range = std::ptr::null_mut();
            let point = UiaPoint { x: 10.0 + 3.0 * 8.0, y: 20.0 + 1.0 * 16.0 };
            assert_eq!((vtable.range_from_point)(raw_provider, point, &mut range), 0);
            assert!(!range.is_null());
            assert_eq!(range_text(range), "second");

            release_range(range);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn range_from_point_clamps_padding_and_outside_points_to_nearest_cell() {
        let provider = RawTextProvider::allocate("first\nsecond\nthird".to_owned());
        unsafe {
            (*provider.as_ptr()).set_test_layout(10.0, 20.0, 8.0, 16.0, 6, 3);
        }

        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut range = std::ptr::null_mut();
            assert_eq!(
                (vtable.range_from_point)(raw_provider, UiaPoint { x: 0.0, y: 0.0 }, &mut range,),
                0,
            );
            assert_eq!(range_text(range), "first");
            release_range(range);

            let mut range = std::ptr::null_mut();
            assert_eq!(
                (vtable.range_from_point)(
                    raw_provider,
                    UiaPoint { x: 10.0 + 20.0 * 8.0, y: 20.0 + 20.0 * 16.0 },
                    &mut range,
                ),
                0,
            );
            assert_eq!(range_text(range), "third");

            release_range(range);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn range_from_point_does_not_reuse_stale_layout_after_layout_is_cleared() {
        let provider = RawTextProvider::allocate("first\nsecond\nthird".to_owned());
        unsafe {
            (*provider.as_ptr()).set_test_layout(10.0, 20.0, 8.0, 16.0, 6, 3);
        }

        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            (*provider.as_ptr()).set_terminal_state(
                crate::accessibility::snapshot::VisibleTerminalSnapshot::from_text_for_tests(
                    "alpha\nbeta\ngamma",
                    6,
                    3,
                ),
                None,
                Vec::new(),
            );

            let mut range = std::ptr::null_mut();
            let point = UiaPoint { x: 10.0 + 3.0 * 8.0, y: 20.0 + 1.0 * 16.0 };
            assert_eq!((vtable.range_from_point)(raw_provider, point, &mut range), 0);
            assert_eq!(range_text(range), "alpha");

            release_range(range);
            (vtable.release)(raw_provider);
        }
    }
}
