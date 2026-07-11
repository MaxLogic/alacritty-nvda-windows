//! Windows UI Automation text range navigation tests.

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::{SysFreeString, SysStringLen};
    use windows_sys::Win32::UI::Accessibility::{
        TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
        TextUnit_Line, TextUnit_Word,
    };
    use windows_sys::core::BSTR;

    use crate::accessibility::text_pattern::RawTextProvider;

    unsafe fn document_range(text: &str) -> (*mut std::ffi::c_void, *mut std::ffi::c_void) {
        let provider = RawTextProvider::allocate(text.to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };
        let mut range = std::ptr::null_mut();
        assert_eq!(unsafe { (vtable.document_range)(raw_provider, &mut range) }, 0);
        (raw_provider, range)
    }

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
    fn compare_endpoints_reports_order() {
        unsafe {
            let (provider, first) = document_range("alpha\nbeta");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let mut second = std::ptr::null_mut();
            assert_eq!((provider_vtable.document_range)(provider, &mut second), 0);

            let first_vtable =
                *(first as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let second_vtable =
                *(second as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;
            assert_eq!(
                (second_vtable.move_endpoint_by_unit)(
                    second,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    3,
                    &mut moved,
                ),
                0,
            );

            let mut comparison = 0;
            assert_eq!(
                (first_vtable.compare_endpoints)(
                    first,
                    TextPatternRangeEndpoint_Start,
                    second,
                    TextPatternRangeEndpoint_Start,
                    &mut comparison,
                ),
                0,
            );
            assert_eq!(comparison, -1);

            release_range(first);
            release_range(second);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_endpoint_by_character_changes_range_text() {
        unsafe {
            let (provider, range) = document_range("abcdef");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    2,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(moved, 2);
            assert_eq!(range_text(range), "cdef");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn unicode_character_move_preserves_boundaries() {
        unsafe {
            let (provider, range) = document_range("aé€😀e\u{301}界z");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -6,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "a");

            for expected in ["é", "€", "😀", "e\u{301}", "界", "z"] {
                assert_eq!((range_vtable.move_range)(range, TextUnit_Character, 1, &mut moved), 0,);
                assert_eq!(moved, 1);
                assert_eq!(range_text(range), expected);
            }

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn unicode_character_move_handles_original_5172_boundary() {
        unsafe {
            let text = format!("{}éz", "a".repeat(5171));
            let (provider, range) = document_range(&text);
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    5170,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -2,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "a");

            assert_eq!((range_vtable.move_range)(range, TextUnit_Character, 1, &mut moved), 0,);
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "é");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn character_move_clips_at_document_end() {
        unsafe {
            let (provider, range) = document_range("az");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    1,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "z");

            assert_eq!((range_vtable.move_range)(range, TextUnit_Character, 1, &mut moved), 0,);
            assert_eq!(moved, 0);
            assert_eq!(range_text(range), "z");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn moving_start_endpoint_past_end_collapses_range_at_new_offset() {
        unsafe {
            let (provider, range) = document_range("abcdef");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -4,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "ab");

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    4,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(moved, 4);
            assert_eq!(range_text(range), "");

            let mut target = std::ptr::null_mut();
            assert_eq!((provider_vtable.document_range)(provider, &mut target), 0);
            let target_vtable =
                *(target as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            assert_eq!(
                (target_vtable.move_endpoint_by_unit)(
                    target,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    4,
                    &mut moved,
                ),
                0,
            );
            let mut comparison = 1;
            assert_eq!(
                (range_vtable.compare_endpoints)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    target,
                    TextPatternRangeEndpoint_Start,
                    &mut comparison,
                ),
                0,
            );
            assert_eq!(comparison, 0);

            release_range(range);
            release_range(target);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_endpoint_by_range_crossing_collapses_range() {
        unsafe {
            let (provider, range) = document_range("abcdef");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut target = std::ptr::null_mut();
            assert_eq!((provider_vtable.document_range)(provider, &mut target), 0);
            let target_vtable =
                *(target as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -4,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (target_vtable.move_endpoint_by_unit)(
                    target,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    4,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_range)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    target,
                    TextPatternRangeEndpoint_Start,
                ),
                0,
            );
            assert_eq!(range_text(range), "");

            let mut comparison = 1;
            assert_eq!(
                (range_vtable.compare_endpoints)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    target,
                    TextPatternRangeEndpoint_Start,
                    &mut comparison,
                ),
                0,
            );
            assert_eq!(comparison, 0);

            release_range(range);
            release_range(target);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn expand_to_line_selects_current_line() {
        unsafe {
            let (provider, range) = document_range("first\nsecond\nthird");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    8,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -5,
                    &mut moved,
                ),
                0,
            );

            assert_eq!((range_vtable.expand_to_enclosing_unit)(range, TextUnit_Line), 0);
            assert_eq!(range_text(range), "second");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_by_word_advances_to_next_word() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!((range_vtable.expand_to_enclosing_unit)(range, TextUnit_Word), 0);
            assert_eq!(range_text(range), "alpha");
            assert_eq!((range_vtable.move_range)(range, TextUnit_Word, 1, &mut moved), 0);
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "beta");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_by_word_from_inside_word_advances_to_next_word() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    2,
                    &mut moved,
                ),
                0,
            );
            assert_eq!((range_vtable.move_range)(range, TextUnit_Word, 1, &mut moved), 0);
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "beta");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_by_word_from_word_end_advances_to_next_word() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    5,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -11,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "");
            assert_eq!((range_vtable.move_range)(range, TextUnit_Word, 1, &mut moved), 0);
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "beta");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn expand_collapsed_word_range_at_word_end_selects_word_ahead() {
        unsafe {
            let (provider, range) = document_range("word1 word2");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    5,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -6,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "");
            assert_eq!((range_vtable.expand_to_enclosing_unit)(range, TextUnit_Word), 0);
            assert_eq!(range_text(range), "word2");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn expand_collapsed_word_range_before_comma_selects_word_ahead() {
        unsafe {
            let (provider, range) = document_range("word1, word2");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    5,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    -7,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(range_text(range), "");
            assert_eq!((range_vtable.expand_to_enclosing_unit)(range, TextUnit_Word), 0);
            assert_eq!(range_text(range), "word2");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_endpoint_by_word_from_inside_word_advances_to_next_word() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    2,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Word,
                    1,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "beta gamma");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_endpoint_by_line_from_inside_line_advances_to_next_line() {
        unsafe {
            let (provider, range) = document_range("first\nsecond\nthird");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    8,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Line,
                    1,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(moved, 1);
            assert_eq!(range_text(range), "third");

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_by_zero_does_not_normalize_or_change_range() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    2,
                    &mut moved,
                ),
                0,
            );
            let before = range_text(range);
            assert_eq!((range_vtable.move_range)(range, TextUnit_Word, 0, &mut moved), 0);
            assert_eq!(moved, 0);
            assert_eq!(range_text(range), before);

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }

    #[test]
    fn move_endpoint_by_zero_does_not_normalize_or_change_range() {
        unsafe {
            let (provider, range) = document_range("alpha beta gamma");
            let provider_vtable =
                (*(provider as *mut crate::accessibility::text_pattern::RawTextProvider)).vtable;
            let range_vtable =
                *(range as *mut &'static crate::accessibility::text_pattern::RawTextRangeVtable);
            let mut moved = 0;

            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Character,
                    2,
                    &mut moved,
                ),
                0,
            );
            let before = range_text(range);
            assert_eq!(
                (range_vtable.move_endpoint_by_unit)(
                    range,
                    TextPatternRangeEndpoint_Start,
                    TextUnit_Word,
                    0,
                    &mut moved,
                ),
                0,
            );
            assert_eq!(moved, 0);
            assert_eq!(range_text(range), before);

            release_range(range);
            (provider_vtable.release)(provider);
        }
    }
}
