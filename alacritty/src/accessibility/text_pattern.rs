//! Windows UI Automation TextPattern provider.

use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;

use windows_sys::core::{BSTR, GUID, HRESULT};
use windows_sys::Win32::Foundation::{SysAllocStringLen, BOOL, E_FAIL, HWND, S_OK};
use windows_sys::Win32::System::Com::SAFEARRAY;
use windows_sys::Win32::System::Ole::{
    SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement, SafeArraySetIID,
};
use windows_sys::Win32::System::Variant::{VariantInit, VARENUM, VARIANT, VT_EMPTY, VT_UNKNOWN};
use windows_sys::Win32::UI::Accessibility::{
    SupportedTextSelection, SupportedTextSelection_None, TextPatternRangeEndpoint, TextUnit,
    UiaPoint, UIA_TEXTATTRIBUTE_ID,
};

use crate::accessibility::windows_provider::{add_ref_raw_provider, release_raw_provider};

const E_INVALIDARG: HRESULT = 0x8007_0057u32 as i32;
const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_NOTIMPL: HRESULT = 0x8000_4001u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_ITEXT_PROVIDER: GUID = GUID::from_u128(0x3589c92c_63f3_4367_99bb_ada653b77cf2);
const IID_ITEXT_RANGE_PROVIDER: GUID = GUID::from_u128(0x5347ad7b_c355_46f8_aff5_909033582f63);

/// UIA TextPattern provider backed by the latest visible terminal text.
#[repr(C)]
pub struct RawTextProvider {
    pub(crate) vtable: &'static RawTextProviderVtable,
    ref_count: AtomicU32,
    text: RwLock<String>,
    hwnd: HWND,
    pub(crate) enclosing_provider: *mut c_void,
}

impl RawTextProvider {
    #[cfg(test)]
    pub(crate) fn allocate(text: String) -> NonNull<Self> {
        Self::allocate_for_window(0 as _, ptr::null_mut(), text)
    }

    pub(crate) fn allocate_for_window(
        hwnd: HWND,
        enclosing_provider: *mut c_void,
        text: String,
    ) -> NonNull<Self> {
        let provider = Box::new(Self {
            vtable: &RAW_TEXT_PROVIDER_VTABLE,
            ref_count: AtomicU32::new(1),
            text: RwLock::new(text),
            hwnd,
            enclosing_provider,
        });
        NonNull::from(Box::leak(provider))
    }

    pub(crate) fn set_text(&self, text: String) {
        *self.text.write().expect("text provider lock poisoned") = text;
    }

    pub(crate) fn disconnect_enclosing_provider(&mut self) {
        self.enclosing_provider = ptr::null_mut();
    }

    fn text(&self) -> String {
        self.text.read().expect("text provider lock poisoned").clone()
    }
}

#[repr(C)]
pub(crate) struct RawTextProviderVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub(crate) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(crate) release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_selection: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    get_visible_ranges: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    range_from_child:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    range_from_point: unsafe extern "system" fn(*mut c_void, UiaPoint, *mut *mut c_void) -> HRESULT,
    pub(crate) document_range: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub(crate) supported_text_selection:
        unsafe extern "system" fn(*mut c_void, *mut SupportedTextSelection) -> HRESULT,
}

static RAW_TEXT_PROVIDER_VTABLE: RawTextProviderVtable = RawTextProviderVtable {
    query_interface: text_provider_query_interface,
    add_ref: text_provider_add_ref,
    release: text_provider_release,
    get_selection: text_provider_get_selection,
    get_visible_ranges: text_provider_get_visible_ranges,
    range_from_child: text_provider_range_from_child,
    range_from_point: text_provider_range_from_point,
    document_range: text_provider_document_range,
    supported_text_selection: text_provider_supported_text_selection,
};

unsafe extern "system" fn text_provider_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> HRESULT {
    if interface.is_null() || iid.is_null() {
        return E_POINTER;
    }

    unsafe {
        *interface = ptr::null_mut();
        if guid_eq(&*iid, &IID_IUNKNOWN) || guid_eq(&*iid, &IID_ITEXT_PROVIDER) {
            text_provider_add_ref(this);
            *interface = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn text_provider_add_ref(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawTextProvider) };
    provider.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) unsafe extern "system" fn text_provider_release(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawTextProvider) };
    let previous = provider.ref_count.fetch_sub(1, Ordering::Release);
    let remaining = previous.saturating_sub(1);

    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe {
            drop(Box::from_raw(this as *mut RawTextProvider));
        }
    }

    remaining
}

unsafe extern "system" fn text_provider_get_selection(
    _this: *mut c_void,
    ranges: *mut *mut SAFEARRAY,
) -> HRESULT {
    if ranges.is_null() {
        return E_POINTER;
    }

    unsafe { *ranges = empty_unknown_safearray() };
    S_OK
}

unsafe extern "system" fn text_provider_get_visible_ranges(
    this: *mut c_void,
    ranges: *mut *mut SAFEARRAY,
) -> HRESULT {
    if ranges.is_null() {
        return E_POINTER;
    }

    unsafe { *ranges = ptr::null_mut() };
    let provider = unsafe { &*(this as *const RawTextProvider) };
    let text = provider.text();
    let end = text.len();
    let range = RawTextRange::allocate(text, 0, end, provider.enclosing_provider);
    let array = unsafe { single_unknown_safearray(range.as_ptr().cast()) };
    unsafe {
        text_range_release(range.as_ptr().cast());
        *ranges = array;
    }

    if array.is_null() {
        E_FAIL
    } else {
        S_OK
    }
}

unsafe extern "system" fn text_provider_range_from_child(
    _this: *mut c_void,
    _child: *mut c_void,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    unsafe { *range = ptr::null_mut() };
    E_INVALIDARG
}

unsafe extern "system" fn text_provider_range_from_point(
    this: *mut c_void,
    _point: UiaPoint,
    range: *mut *mut c_void,
) -> HRESULT {
    unsafe { text_provider_document_range(this, range) }
}

unsafe extern "system" fn text_provider_document_range(
    this: *mut c_void,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawTextProvider) };
    let text = provider.text();
    let text_range =
        RawTextRange::allocate(text.clone(), 0, text.len(), provider.enclosing_provider);
    unsafe { *range = text_range.as_ptr().cast() };
    S_OK
}

unsafe extern "system" fn text_provider_supported_text_selection(
    _this: *mut c_void,
    selection: *mut SupportedTextSelection,
) -> HRESULT {
    if selection.is_null() {
        return E_POINTER;
    }

    unsafe { *selection = SupportedTextSelection_None };
    S_OK
}

#[repr(C)]
pub(crate) struct RawTextRange {
    pub(crate) vtable: &'static RawTextRangeVtable,
    ref_count: AtomicU32,
    text: String,
    start: usize,
    end: usize,
    enclosing_provider: *mut c_void,
}

impl RawTextRange {
    fn allocate(
        text: String,
        start: usize,
        end: usize,
        enclosing_provider: *mut c_void,
    ) -> NonNull<Self> {
        unsafe {
            add_ref_raw_provider(enclosing_provider);
        }
        let range = Box::new(Self {
            vtable: &RAW_TEXT_RANGE_VTABLE,
            ref_count: AtomicU32::new(1),
            text,
            start,
            end,
            enclosing_provider,
        });
        NonNull::from(Box::leak(range))
    }

    fn selected_text(&self, max_length: i32) -> String {
        let text = &self.text[self.start..self.end];
        if max_length < 0 {
            text.to_owned()
        } else {
            text.chars().take(max_length as usize).collect()
        }
    }
}

impl Drop for RawTextRange {
    fn drop(&mut self) {
        unsafe {
            release_raw_provider(self.enclosing_provider);
        }
    }
}

#[repr(C)]
pub(crate) struct RawTextRangeVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(crate) release: unsafe extern "system" fn(*mut c_void) -> u32,
    clone: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    compare: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut BOOL) -> HRESULT,
    compare_endpoints: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut i32,
    ) -> HRESULT,
    expand_to_enclosing_unit: unsafe extern "system" fn(*mut c_void, TextUnit) -> HRESULT,
    find_attribute: unsafe extern "system" fn(
        *mut c_void,
        UIA_TEXTATTRIBUTE_ID,
        VARIANT,
        BOOL,
        *mut *mut c_void,
    ) -> HRESULT,
    find_text:
        unsafe extern "system" fn(*mut c_void, BSTR, BOOL, BOOL, *mut *mut c_void) -> HRESULT,
    get_attribute_value:
        unsafe extern "system" fn(*mut c_void, UIA_TEXTATTRIBUTE_ID, *mut VARIANT) -> HRESULT,
    get_bounding_rectangles: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub(crate) get_enclosing_element:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub(crate) get_text: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
    move_range: unsafe extern "system" fn(*mut c_void, TextUnit, i32, *mut i32) -> HRESULT,
    move_endpoint_by_unit: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        TextUnit,
        i32,
        *mut i32,
    ) -> HRESULT,
    move_endpoint_by_range: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut c_void,
        TextPatternRangeEndpoint,
    ) -> HRESULT,
    select: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    add_to_selection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    remove_from_selection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    scroll_into_view: unsafe extern "system" fn(*mut c_void, BOOL) -> HRESULT,
    get_children: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
}

static RAW_TEXT_RANGE_VTABLE: RawTextRangeVtable = RawTextRangeVtable {
    query_interface: text_range_query_interface,
    add_ref: text_range_add_ref,
    release: text_range_release,
    clone: text_range_clone,
    compare: text_range_compare,
    compare_endpoints: text_range_compare_endpoints,
    expand_to_enclosing_unit: text_range_expand_to_enclosing_unit,
    find_attribute: text_range_find_attribute,
    find_text: text_range_find_text,
    get_attribute_value: text_range_get_attribute_value,
    get_bounding_rectangles: text_range_get_bounding_rectangles,
    get_enclosing_element: text_range_get_enclosing_element,
    get_text: text_range_get_text,
    move_range: text_range_move,
    move_endpoint_by_unit: text_range_move_endpoint_by_unit,
    move_endpoint_by_range: text_range_move_endpoint_by_range,
    select: text_range_select,
    add_to_selection: text_range_add_to_selection,
    remove_from_selection: text_range_remove_from_selection,
    scroll_into_view: text_range_scroll_into_view,
    get_children: text_range_get_children,
};

unsafe extern "system" fn text_range_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> HRESULT {
    if interface.is_null() || iid.is_null() {
        return E_POINTER;
    }

    unsafe {
        *interface = ptr::null_mut();
        if guid_eq(&*iid, &IID_IUNKNOWN) || guid_eq(&*iid, &IID_ITEXT_RANGE_PROVIDER) {
            text_range_add_ref(this);
            *interface = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn text_range_add_ref(this: *mut c_void) -> u32 {
    let range = unsafe { &*(this as *const RawTextRange) };
    range.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) unsafe extern "system" fn text_range_release(this: *mut c_void) -> u32 {
    let range = unsafe { &*(this as *const RawTextRange) };
    let previous = range.ref_count.fetch_sub(1, Ordering::Release);
    let remaining = previous.saturating_sub(1);

    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe {
            drop(Box::from_raw(this as *mut RawTextRange));
        }
    }

    remaining
}

unsafe extern "system" fn text_range_clone(this: *mut c_void, range: *mut *mut c_void) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    let source = unsafe { &*(this as *const RawTextRange) };
    let clone = RawTextRange::allocate(
        source.text.clone(),
        source.start,
        source.end,
        source.enclosing_provider,
    );
    unsafe { *range = clone.as_ptr().cast() };
    S_OK
}

unsafe extern "system" fn text_range_compare(
    this: *mut c_void,
    other: *mut c_void,
    equal: *mut BOOL,
) -> HRESULT {
    if equal.is_null() {
        return E_POINTER;
    }
    if other.is_null() {
        unsafe { *equal = 0 };
        return S_OK;
    }

    let left = unsafe { &*(this as *const RawTextRange) };
    let right = unsafe { &*(other as *const RawTextRange) };
    unsafe {
        *equal =
            (left.start == right.start && left.end == right.end && left.text == right.text) as BOOL;
    }
    S_OK
}

unsafe extern "system" fn text_range_compare_endpoints(
    _this: *mut c_void,
    _endpoint: TextPatternRangeEndpoint,
    _target_range: *mut c_void,
    _target_endpoint: TextPatternRangeEndpoint,
    comparison: *mut i32,
) -> HRESULT {
    if comparison.is_null() {
        return E_POINTER;
    }

    unsafe { *comparison = 0 };
    S_OK
}

unsafe extern "system" fn text_range_expand_to_enclosing_unit(
    _this: *mut c_void,
    _unit: TextUnit,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn text_range_find_attribute(
    _this: *mut c_void,
    _attribute_id: UIA_TEXTATTRIBUTE_ID,
    _value: VARIANT,
    _backward: BOOL,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    unsafe { *range = ptr::null_mut() };
    S_OK
}

unsafe extern "system" fn text_range_find_text(
    _this: *mut c_void,
    _text: BSTR,
    _backward: BOOL,
    _ignore_case: BOOL,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    unsafe { *range = ptr::null_mut() };
    S_OK
}

unsafe extern "system" fn text_range_get_attribute_value(
    _this: *mut c_void,
    _attribute_id: UIA_TEXTATTRIBUTE_ID,
    value: *mut VARIANT,
) -> HRESULT {
    if value.is_null() {
        return E_POINTER;
    }

    unsafe {
        VariantInit(value);
        (*value).Anonymous.Anonymous.vt = VT_EMPTY;
    }
    S_OK
}

unsafe extern "system" fn text_range_get_bounding_rectangles(
    _this: *mut c_void,
    rectangles: *mut *mut SAFEARRAY,
) -> HRESULT {
    if rectangles.is_null() {
        return E_POINTER;
    }

    unsafe { *rectangles = empty_array(VT_EMPTY) };
    S_OK
}

unsafe extern "system" fn text_range_get_enclosing_element(
    this: *mut c_void,
    provider: *mut *mut c_void,
) -> HRESULT {
    if provider.is_null() {
        return E_POINTER;
    }

    let range = unsafe { &*(this as *const RawTextRange) };
    unsafe {
        add_ref_raw_provider(range.enclosing_provider);
        *provider = range.enclosing_provider;
    }
    S_OK
}

unsafe extern "system" fn text_range_get_text(
    this: *mut c_void,
    max_length: i32,
    text: *mut BSTR,
) -> HRESULT {
    if text.is_null() {
        return E_POINTER;
    }

    let range = unsafe { &*(this as *const RawTextRange) };
    let selected_text = range.selected_text(max_length);
    unsafe { *text = string_to_bstr(&selected_text) };
    S_OK
}

unsafe extern "system" fn text_range_move(
    _this: *mut c_void,
    _unit: TextUnit,
    _count: i32,
    moved: *mut i32,
) -> HRESULT {
    if moved.is_null() {
        return E_POINTER;
    }

    unsafe { *moved = 0 };
    S_OK
}

unsafe extern "system" fn text_range_move_endpoint_by_unit(
    _this: *mut c_void,
    _endpoint: TextPatternRangeEndpoint,
    _unit: TextUnit,
    _count: i32,
    moved: *mut i32,
) -> HRESULT {
    if moved.is_null() {
        return E_POINTER;
    }

    unsafe { *moved = 0 };
    S_OK
}

unsafe extern "system" fn text_range_move_endpoint_by_range(
    _this: *mut c_void,
    _endpoint: TextPatternRangeEndpoint,
    _target_range: *mut c_void,
    _target_endpoint: TextPatternRangeEndpoint,
) -> HRESULT {
    S_OK
}

unsafe extern "system" fn text_range_select(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn text_range_add_to_selection(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn text_range_remove_from_selection(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn text_range_scroll_into_view(_this: *mut c_void, _align: BOOL) -> HRESULT {
    S_OK
}

unsafe extern "system" fn text_range_get_children(
    _this: *mut c_void,
    children: *mut *mut SAFEARRAY,
) -> HRESULT {
    if children.is_null() {
        return E_POINTER;
    }

    unsafe { *children = empty_unknown_safearray() };
    S_OK
}

unsafe fn single_unknown_safearray(value: *mut c_void) -> *mut SAFEARRAY {
    let array = unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, 1) };
    if array.is_null() {
        return ptr::null_mut();
    }

    let _ = unsafe { SafeArraySetIID(array, &IID_IUNKNOWN) };
    let index = 0;
    if unsafe { SafeArrayPutElement(array, &index, value as *const c_void) } != S_OK {
        unsafe {
            SafeArrayDestroy(array);
        }
        return ptr::null_mut();
    }

    array
}

fn empty_unknown_safearray() -> *mut SAFEARRAY {
    unsafe {
        let array = SafeArrayCreateVector(VT_UNKNOWN, 0, 0);
        if !array.is_null() {
            let _ = SafeArraySetIID(array, &IID_IUNKNOWN);
        }
        array
    }
}

fn empty_array(vartype: VARENUM) -> *mut SAFEARRAY {
    unsafe { SafeArrayCreateVector(vartype, 0, 0) }
}

fn string_to_bstr(value: &str) -> BSTR {
    let wide: Vec<u16> = value.encode_utf16().collect();
    unsafe { SysAllocStringLen(wide.as_ptr(), wide.len() as u32) }
}

fn guid_eq(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

#[cfg(test)]
mod tests {
    use windows_sys::core::BSTR;
    use windows_sys::Win32::Foundation::{SysFreeString, SysStringLen};
    use windows_sys::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement};
    use windows_sys::Win32::UI::Accessibility::SupportedTextSelection_None;

    use super::RawTextProvider;

    #[test]
    fn document_range_get_text_returns_visible_text() {
        let provider = RawTextProvider::allocate("first line\nsecond line".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut range: *mut std::ffi::c_void = std::ptr::null_mut();
            assert_eq!((vtable.document_range)(raw_provider, &mut range), 0);
            assert!(!range.is_null());

            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            let mut text: BSTR = std::ptr::null_mut();
            assert_eq!((range_vtable.get_text)(range, -1, &mut text), 0);
            assert_eq!(
                String::from_utf16_lossy(std::slice::from_raw_parts(
                    text,
                    SysStringLen(text) as usize,
                )),
                "first line\nsecond line"
            );

            SysFreeString(text);
            (range_vtable.release)(range);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_provider_reports_no_selection_until_selection_support_exists() {
        let provider = RawTextProvider::allocate("text".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut selection = 1;
            assert_eq!((vtable.supported_text_selection)(raw_provider, &mut selection), 0);
            assert_eq!(selection, SupportedTextSelection_None);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn visible_ranges_returns_viewport_text_range() {
        let provider = RawTextProvider::allocate("visible viewport".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut ranges = std::ptr::null_mut();
            assert_eq!((vtable.get_visible_ranges)(raw_provider, &mut ranges), 0);
            assert!(!ranges.is_null());

            let mut range: *mut std::ffi::c_void = std::ptr::null_mut();
            let index = 0;
            assert_eq!(SafeArrayGetElement(ranges, &index, &mut range as *mut _ as *mut _), 0);
            assert!(!range.is_null());

            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            let mut text: BSTR = std::ptr::null_mut();
            assert_eq!((range_vtable.get_text)(range, -1, &mut text), 0);
            assert_eq!(
                String::from_utf16_lossy(std::slice::from_raw_parts(
                    text,
                    SysStringLen(text) as usize,
                )),
                "visible viewport"
            );

            SysFreeString(text);
            (range_vtable.release)(range);
            SafeArrayDestroy(ranges);
            (vtable.release)(raw_provider);
        }
    }
}
