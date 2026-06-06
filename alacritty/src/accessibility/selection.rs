//! Windows UI Automation selection tests.

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::{SysFreeString, SysStringLen};
    use windows_sys::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement};
    use windows_sys::Win32::UI::Accessibility::SupportedTextSelection_Multiple;
    use windows_sys::core::BSTR;

    use crate::accessibility::text_pattern::RawTextProvider;

    unsafe fn range_text(range: *mut std::ffi::c_void) -> String {
        let vtable = unsafe {
            *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable)
        };
        let mut text: BSTR = std::ptr::null_mut();
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
    fn get_selection_returns_selected_text_range() {
        let provider = RawTextProvider::allocate("first\nsecond\nthird".to_owned());
        unsafe {
            (*provider.as_ptr()).set_test_selection(6, 12);
        }

        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut ranges = std::ptr::null_mut();
            assert_eq!((vtable.get_selection)(raw_provider, &mut ranges), 0);
            assert!(!ranges.is_null());

            let mut range: *mut std::ffi::c_void = std::ptr::null_mut();
            let index = 0;
            assert_eq!(SafeArrayGetElement(ranges, &index, &mut range as *mut _ as *mut _), 0);
            assert_eq!(range_text(range), "second");

            release_range(range);
            SafeArrayDestroy(ranges);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn get_selection_returns_multiple_selected_ranges() {
        let provider = RawTextProvider::allocate("ab cd\nef gh".to_owned());
        unsafe {
            (*provider.as_ptr()).set_test_selections(vec![(0, 2), (6, 8)]);
        }

        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut ranges = std::ptr::null_mut();
            assert_eq!((vtable.get_selection)(raw_provider, &mut ranges), 0);
            assert!(!ranges.is_null());

            let mut first: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut second: *mut std::ffi::c_void = std::ptr::null_mut();
            assert_eq!(SafeArrayGetElement(ranges, &0, &mut first as *mut _ as *mut _), 0);
            assert_eq!(SafeArrayGetElement(ranges, &1, &mut second as *mut _ as *mut _), 0);
            assert_eq!(range_text(first), "ab");
            assert_eq!(range_text(second), "ef");

            release_range(first);
            release_range(second);
            SafeArrayDestroy(ranges);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn get_selection_returns_empty_array_without_selection() {
        let provider = RawTextProvider::allocate("first\nsecond".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut ranges = std::ptr::null_mut();
            assert_eq!((vtable.get_selection)(raw_provider, &mut ranges), 0);
            assert!(!ranges.is_null());

            let mut range: *mut std::ffi::c_void = std::ptr::null_mut();
            let index = 0;
            assert_ne!(SafeArrayGetElement(ranges, &index, &mut range as *mut _ as *mut _), 0);

            SafeArrayDestroy(ranges);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_provider_reports_multiple_selection_support() {
        let provider = RawTextProvider::allocate("text".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut selection = 0;
            assert_eq!((vtable.supported_text_selection)(raw_provider, &mut selection), 0);
            assert_eq!(selection, SupportedTextSelection_Multiple);
            (vtable.release)(raw_provider);
        }
    }
}
