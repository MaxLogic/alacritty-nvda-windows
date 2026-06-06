//! Windows UI Automation provider attachment.

use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::core::{BSTR, GUID, HRESULT};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, S_OK, VARIANT_TRUE, WPARAM};
use windows_sys::Win32::System::Variant::{
    VariantInit, VARENUM, VARIANT, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I4,
};
use windows_sys::Win32::UI::Accessibility::{
    ProviderOptions, ProviderOptions_ServerSideProvider, UIA_ControlTypePropertyId,
    UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
    UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId, UIA_TextControlTypeId,
    UiaDisconnectProvider, UiaHostProviderFromHwnd, UiaReturnRawElementProvider, UIA_PROPERTY_ID,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{OBJID_CLIENT, WM_GETOBJECT};
use winit::raw_window_handle::RawWindowHandle;

const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IRAW_ELEMENT_PROVIDER_SIMPLE: GUID =
    GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
const SUBCLASS_ID: usize = 1;

/// Minimal UIA provider state for an Alacritty terminal window.
#[derive(Clone, Debug)]
pub struct TerminalProvider {
    name: String,
}

impl TerminalProvider {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn provider_options(&self) -> ProviderOptions {
        ProviderOptions_ServerSideProvider
    }

    pub fn property_i4(&self, property_id: UIA_PROPERTY_ID) -> Option<i32> {
        match property_id {
            id if id == UIA_ControlTypePropertyId => Some(UIA_TextControlTypeId),
            _ => None,
        }
    }

    pub fn property_bstr(&self, property_id: UIA_PROPERTY_ID) -> Option<String> {
        match property_id {
            id if id == UIA_NamePropertyId => Some(self.name.clone()),
            _ => None,
        }
    }

    pub fn property_bool(&self, property_id: UIA_PROPERTY_ID) -> Option<bool> {
        match property_id {
            id if id == UIA_IsControlElementPropertyId
                || id == UIA_IsContentElementPropertyId
                || id == UIA_IsKeyboardFocusablePropertyId =>
            {
                Some(VARIANT_TRUE != 0)
            },
            _ => None,
        }
    }

    pub fn property_variant_type(&self, property_id: UIA_PROPERTY_ID) -> Option<VARENUM> {
        match property_id {
            id if id == UIA_ControlTypePropertyId => Some(VT_I4),
            id if id == UIA_NamePropertyId => Some(VT_BSTR),
            id if id == UIA_IsControlElementPropertyId
                || id == UIA_IsContentElementPropertyId
                || id == UIA_IsKeyboardFocusablePropertyId =>
            {
                Some(VT_BOOL)
            },
            _ => Some(VT_EMPTY),
        }
    }
}

/// Per-window accessibility attachment.
#[derive(Debug)]
pub struct WindowsAccessibility {
    hwnd: HWND,
    provider: NonNull<RawProvider>,
}

impl WindowsAccessibility {
    /// Attach a UIA provider to a Win32 window.
    ///
    /// # Safety
    ///
    /// `hwnd` must be a valid Alacritty window handle owned by the current UI thread, and the
    /// returned attachment must be dropped before the window is destroyed.
    pub unsafe fn new(hwnd: HWND, name: impl Into<String>) -> Option<Self> {
        let provider = RawProvider::allocate(hwnd, TerminalProvider::new(name));
        let ref_data = provider.as_ptr() as usize;
        let installed = unsafe {
            SetWindowSubclass(hwnd, Some(accessibility_subclass_proc), SUBCLASS_ID, ref_data)
        };

        if installed == 0 {
            unsafe {
                release(provider.as_ptr().cast());
            }
            None
        } else {
            Some(Self { hwnd, provider })
        }
    }

    pub fn raw_provider(&self) -> *mut c_void {
        self.provider.as_ptr().cast()
    }
}

impl Drop for WindowsAccessibility {
    fn drop(&mut self) {
        unsafe {
            UiaDisconnectProvider(self.raw_provider());
            RemoveWindowSubclass(self.hwnd, Some(accessibility_subclass_proc), SUBCLASS_ID);
            release(self.raw_provider());
        }
    }
}

/// Whether a Windows message is a UIA client-object provider request.
pub fn should_handle_wm_getobject(message: u32, lparam: isize) -> bool {
    message == WM_GETOBJECT && lparam == OBJID_CLIENT as isize
}

pub fn hwnd_from_raw_window_handle(raw_window_handle: RawWindowHandle) -> Option<HWND> {
    match raw_window_handle {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

unsafe extern "system" fn accessibility_subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    ref_data: usize,
) -> LRESULT {
    if should_handle_wm_getobject(message, lparam) {
        let provider = ref_data as *mut RawProvider;
        if !provider.is_null() {
            return unsafe {
                UiaReturnRawElementProvider(hwnd, wparam, lparam, (*provider).as_raw())
            };
        }
    }

    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[repr(C)]
#[derive(Debug)]
struct RawProvider {
    vtable: &'static RawProviderVtable,
    ref_count: AtomicU32,
    hwnd: HWND,
    provider: TerminalProvider,
    #[cfg(test)]
    drop_counter: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

impl RawProvider {
    fn new(hwnd: HWND, provider: TerminalProvider) -> Self {
        Self {
            vtable: &RAW_PROVIDER_VTABLE,
            ref_count: AtomicU32::new(1),
            hwnd,
            provider,
            #[cfg(test)]
            drop_counter: None,
        }
    }

    fn allocate(hwnd: HWND, provider: TerminalProvider) -> NonNull<Self> {
        let provider = Box::new(Self::new(hwnd, provider));
        NonNull::from(Box::leak(provider))
    }

    #[cfg(test)]
    fn allocate_with_drop_counter(
        hwnd: HWND,
        provider: TerminalProvider,
        drop_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> NonNull<Self> {
        let mut provider = Box::new(Self::new(hwnd, provider));
        provider.drop_counter = Some(drop_counter);
        NonNull::from(Box::leak(provider))
    }

    fn as_raw(&self) -> *mut c_void {
        self as *const Self as *mut c_void
    }
}

#[cfg(test)]
impl Drop for RawProvider {
    fn drop(&mut self) {
        if let Some(drop_counter) = &self.drop_counter {
            drop_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[repr(C)]
struct RawProviderVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    provider_options: unsafe extern "system" fn(*mut c_void, *mut ProviderOptions) -> HRESULT,
    get_pattern_provider: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    get_property_value:
        unsafe extern "system" fn(*mut c_void, UIA_PROPERTY_ID, *mut VARIANT) -> HRESULT,
    host_raw_element_provider: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

impl std::fmt::Debug for RawProviderVtable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RawProviderVtable")
    }
}

static RAW_PROVIDER_VTABLE: RawProviderVtable = RawProviderVtable {
    query_interface,
    add_ref,
    release,
    provider_options,
    get_pattern_provider,
    get_property_value,
    host_raw_element_provider,
};

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> HRESULT {
    if interface.is_null() || iid.is_null() {
        return E_POINTER;
    }

    unsafe {
        *interface = ptr::null_mut();
        if guid_eq(&*iid, &IID_IUNKNOWN) || guid_eq(&*iid, &IID_IRAW_ELEMENT_PROVIDER_SIMPLE) {
            add_ref(this);
            *interface = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawProvider) };
    provider.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawProvider) };
    let previous = provider.ref_count.fetch_sub(1, Ordering::Release);
    let remaining = previous.saturating_sub(1);

    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe {
            drop(Box::from_raw(this as *mut RawProvider));
        }
    }

    remaining
}

unsafe extern "system" fn provider_options(
    this: *mut c_void,
    options: *mut ProviderOptions,
) -> HRESULT {
    if options.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawProvider) };
    unsafe {
        *options = provider.provider.provider_options();
    }
    S_OK
}

unsafe extern "system" fn get_pattern_provider(
    _this: *mut c_void,
    _pattern_id: i32,
    pattern_provider: *mut *mut c_void,
) -> HRESULT {
    if pattern_provider.is_null() {
        return E_POINTER;
    }

    unsafe {
        *pattern_provider = ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn get_property_value(
    this: *mut c_void,
    property_id: UIA_PROPERTY_ID,
    value: *mut VARIANT,
) -> HRESULT {
    if value.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawProvider) };
    unsafe {
        VariantInit(value);
        let variant = &mut (*value).Anonymous.Anonymous;

        if let Some(property_value) = provider.provider.property_i4(property_id) {
            variant.vt = VT_I4;
            variant.Anonymous.lVal = property_value;
        } else if let Some(property_value) = provider.provider.property_bstr(property_id) {
            variant.vt = VT_BSTR;
            variant.Anonymous.bstrVal = string_to_bstr(&property_value);
        } else if let Some(property_value) = provider.provider.property_bool(property_id) {
            variant.vt = VT_BOOL;
            variant.Anonymous.boolVal = if property_value { VARIANT_TRUE } else { 0 };
        }
    }

    S_OK
}

unsafe extern "system" fn host_raw_element_provider(
    this: *mut c_void,
    provider: *mut *mut c_void,
) -> HRESULT {
    if provider.is_null() {
        return E_POINTER;
    }

    let raw_provider = unsafe { &*(this as *const RawProvider) };
    unsafe { UiaHostProviderFromHwnd(raw_provider.hwnd, provider) }
}

fn guid_eq(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

fn string_to_bstr(value: &str) -> BSTR {
    let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
    unsafe { windows_sys::Win32::Foundation::SysAllocString(wide.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroIsize;

    use winit::raw_window_handle::{RawWindowHandle, Win32WindowHandle};

    use windows_sys::Win32::Foundation::VARIANT_TRUE;
    use windows_sys::Win32::System::Variant::{VariantClear, VT_BOOL, VT_BSTR, VT_I4};
    use windows_sys::Win32::UI::Accessibility::{
        ProviderOptions_ServerSideProvider, UIA_ControlTypePropertyId,
        UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
        UIA_IsKeyboardFocusablePropertyId, UIA_NamePropertyId, UIA_TextControlTypeId,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{OBJID_CLIENT, WM_GETOBJECT};

    use super::{
        hwnd_from_raw_window_handle, should_handle_wm_getobject, RawProvider, TerminalProvider,
    };

    #[test]
    fn windows_provider_exposes_terminal_properties() {
        let provider = TerminalProvider::new("Alacritty");

        assert_eq!(provider.provider_options(), ProviderOptions_ServerSideProvider);
        assert_eq!(provider.property_i4(UIA_ControlTypePropertyId), Some(UIA_TextControlTypeId));
        assert_eq!(provider.property_bstr(UIA_NamePropertyId).as_deref(), Some("Alacritty"));
        assert_eq!(provider.property_bool(UIA_IsControlElementPropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsContentElementPropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsKeyboardFocusablePropertyId), Some(true));
        assert_eq!(provider.property_variant_type(UIA_ControlTypePropertyId), Some(VT_I4));
        assert_eq!(provider.property_variant_type(UIA_NamePropertyId), Some(VT_BSTR));
        assert_eq!(
            provider.property_variant_type(UIA_IsKeyboardFocusablePropertyId),
            Some(VT_BOOL)
        );
    }

    #[test]
    fn wm_getobject_handler_matches_uia_client_object_request() {
        assert!(should_handle_wm_getobject(WM_GETOBJECT, OBJID_CLIENT as isize));
        assert!(!should_handle_wm_getobject(WM_GETOBJECT, 0));
        assert!(!should_handle_wm_getobject(0, OBJID_CLIENT as isize));
    }

    #[test]
    fn extracts_hwnd_from_win32_raw_window_handle() {
        let hwnd = NonZeroIsize::new(42).unwrap();
        let mut handle = Win32WindowHandle::new(hwnd);
        handle.hinstance = NonZeroIsize::new(7);

        assert_eq!(hwnd_from_raw_window_handle(RawWindowHandle::Win32(handle)), Some(42 as _));
    }

    #[test]
    fn raw_provider_vtable_exposes_options_and_properties() {
        let provider = RawProvider::new(42 as _, TerminalProvider::new("Alacritty"));
        let raw_provider = provider.as_raw();

        unsafe {
            let mut options = 0;
            assert_eq!((provider.vtable.provider_options)(raw_provider, &mut options), 0);
            assert_eq!(options, ProviderOptions_ServerSideProvider);

            let mut control_type = std::mem::zeroed();
            assert_eq!(
                (provider.vtable.get_property_value)(
                    raw_provider,
                    UIA_ControlTypePropertyId,
                    &mut control_type,
                ),
                0,
            );
            assert_eq!(control_type.Anonymous.Anonymous.vt, VT_I4);
            assert_eq!(control_type.Anonymous.Anonymous.Anonymous.lVal, UIA_TextControlTypeId);

            let mut focusable = std::mem::zeroed();
            assert_eq!(
                (provider.vtable.get_property_value)(
                    raw_provider,
                    UIA_IsKeyboardFocusablePropertyId,
                    &mut focusable,
                ),
                0,
            );
            assert_eq!(focusable.Anonymous.Anonymous.vt, VT_BOOL);
            assert_eq!(focusable.Anonymous.Anonymous.Anonymous.boolVal, VARIANT_TRUE);

            let mut name = std::mem::zeroed();
            assert_eq!(
                (provider.vtable.get_property_value)(raw_provider, UIA_NamePropertyId, &mut name),
                0,
            );
            assert_eq!(name.Anonymous.Anonymous.vt, VT_BSTR);

            VariantClear(&mut control_type);
            VariantClear(&mut focusable);
            VariantClear(&mut name);
        }
    }

    #[test]
    fn raw_provider_release_drops_after_last_reference() {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = RawProvider::allocate_with_drop_counter(
            42 as _,
            TerminalProvider::new("Alacritty"),
            std::sync::Arc::clone(&dropped),
        );
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            assert_eq!((vtable.add_ref)(raw_provider), 2);
            assert_eq!((vtable.release)(raw_provider), 1);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!((vtable.release)(raw_provider), 0);
        }

        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
