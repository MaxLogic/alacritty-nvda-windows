//! Windows UI Automation provider attachment.

use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::index::Point;
use alacritty_terminal::term::{Term, TermMode};
use windows_sys::Win32::Foundation::{
    HWND, LPARAM, LRESULT, POINT, RECT, S_OK, SysFreeString, VARIANT_TRUE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::Com::SAFEARRAY;
use windows_sys::Win32::System::Variant::{
    VARENUM, VARIANT, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I4, VariantInit,
};
use windows_sys::Win32::UI::Accessibility::{
    NotificationKind_ItemAdded, NotificationProcessing_CurrentThenMostRecent, ProviderOptions,
    ProviderOptions_ServerSideProvider, UIA_ActiveTextPositionChangedEventId,
    UIA_ControlTypePropertyId, UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
    UIA_IsKeyboardFocusablePropertyId, UIA_IsTextPattern2AvailablePropertyId,
    UIA_IsTextPatternAvailablePropertyId, UIA_NamePropertyId, UIA_NotificationEventId,
    UIA_PROPERTY_ID, UIA_Text_TextChangedEventId, UIA_Text_TextSelectionChangedEventId,
    UIA_TextControlTypeId, UIA_TextPattern2Id, UIA_TextPatternId, UiaClientsAreListening,
    UiaDisconnectProvider, UiaHostProviderFromHwnd, UiaRaiseActiveTextPositionChangedEvent,
    UiaRaiseAutomationEvent, UiaRaiseNotificationEvent, UiaReturnRawElementProvider,
    UiaRootObjectId,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, OBJID_CLIENT, WM_GETOBJECT};
use windows_sys::core::{BSTR, GUID, HRESULT};
use winit::raw_window_handle::RawWindowHandle;

use crate::accessibility::snapshot::VisibleTerminalSnapshot;
use crate::accessibility::text_pattern::{self, RawTextProvider, TextProviderLayout};
use crate::display::SizeInfo;

const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IRAW_ELEMENT_PROVIDER_ADVISE_EVENTS: GUID =
    GUID::from_u128(0xa407b27b_0f6d_4427_9292_473c7bf93258);
const IID_IRAW_ELEMENT_PROVIDER_SIMPLE: GUID =
    GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
const SUBCLASS_ID: usize = 1;
const UIA_EVENT_THROTTLE: Duration = Duration::from_millis(75);
const MAX_NOTIFICATION_CHARS: usize = 4000;

fn trace_uia(message: &str) {
    if let Ok(path) = std::env::var("ALACRITTY_UIA_DEBUG_TRACE") {
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(
            |mut file| {
                use std::io::Write;
                writeln!(file, "{message}")
            },
        );
    }
}

pub(crate) trait UiaEventSink {
    fn clients_are_listening(&self) -> bool;

    fn raise_event(&mut self, provider: *mut c_void, event_id: i32);

    fn raise_active_text_position_changed(&mut self, provider: *mut c_void);

    fn raise_notification(&mut self, provider: *mut c_void, text: &str);
}

#[derive(Debug)]
pub(crate) struct NativeUiaEventSink {
    provider: NonNull<RawProvider>,
}

impl UiaEventSink for NativeUiaEventSink {
    fn clients_are_listening(&self) -> bool {
        unsafe {
            UiaClientsAreListening() != 0 || (*self.provider.as_ptr()).has_advised_event_listeners()
        }
    }

    fn raise_event(&mut self, provider: *mut c_void, event_id: i32) {
        trace_uia(&format!("raise_event {event_id}"));
        let _ = unsafe { UiaRaiseAutomationEvent(provider, event_id) };
        write_event_trace(event_id);
    }

    fn raise_active_text_position_changed(&mut self, provider: *mut c_void) {
        let range = unsafe { (*self.provider.as_ptr()).caret_range() };
        if range.is_null() {
            return;
        }

        trace_uia("raise_active_text_position_changed 20036");
        let _ = unsafe { UiaRaiseActiveTextPositionChangedEvent(provider, range) };
        write_event_trace(UIA_ActiveTextPositionChangedEventId);
        unsafe {
            text_pattern::release_text_range(range);
        }
    }

    fn raise_notification(&mut self, provider: *mut c_void, text: &str) {
        if text.trim().is_empty() {
            return;
        }

        trace_uia(&format!("raise_notification 20035 len={} text={text:?}", text.len()));
        let display_string = string_to_bstr(text);
        let activity_id = string_to_bstr("alacritty-terminal-output");
        let _ = unsafe {
            UiaRaiseNotificationEvent(
                provider,
                NotificationKind_ItemAdded,
                NotificationProcessing_CurrentThenMostRecent,
                display_string,
                activity_id,
            )
        };
        write_event_trace(UIA_NotificationEventId);
        unsafe {
            SysFreeString(display_string);
            SysFreeString(activity_id);
        }
    }
}

fn write_event_trace(event_id: i32) {
    if let Ok(path) = std::env::var("ALACRITTY_UIA_EVENT_TRACE") {
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(
            |mut file| {
                use std::io::Write;
                writeln!(file, "{event_id}")
            },
        );
    }
}

#[derive(Clone, Debug)]
struct PublishedSnapshotState {
    text: String,
    cursor: Point<usize>,
    selection: Vec<(usize, usize)>,
}

#[derive(Debug)]
pub(crate) struct UiaEventThrottle {
    interval: Duration,
    last_emit: Instant,
    has_emitted: bool,
    pending_text: bool,
    pending_selection: bool,
    pending_active_text_position: bool,
    pending_notification: Option<String>,
}

impl UiaEventThrottle {
    pub(crate) fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval,
            last_emit: now,
            has_emitted: false,
            pending_text: false,
            pending_selection: false,
            pending_active_text_position: false,
            pending_notification: None,
        }
    }

    pub(crate) fn record_snapshot_change(
        &mut self,
        text_changed: bool,
        selection_changed: bool,
        active_text_position_changed: bool,
        notification: Option<String>,
    ) {
        self.pending_text |= text_changed;
        self.pending_selection |= selection_changed;
        self.pending_active_text_position |= active_text_position_changed;
        self.record_notification(notification);
    }

    pub(crate) fn flush_due<S: UiaEventSink>(
        &mut self,
        provider: *mut c_void,
        now: Instant,
        sink: &mut S,
    ) {
        if !self.pending_text
            && !self.pending_selection
            && !self.pending_active_text_position
            && self.pending_notification.is_none()
        {
            return;
        }

        if !sink.clients_are_listening() {
            self.pending_text = false;
            self.pending_selection = false;
            self.pending_active_text_position = false;
            self.pending_notification = None;
            self.last_emit = now;
            return;
        }

        if self.has_emitted && now.duration_since(self.last_emit) < self.interval {
            if self.pending_active_text_position {
                sink.raise_active_text_position_changed(provider);
                self.pending_active_text_position = false;
            }
            if let Some(notification) = self.pending_notification.take() {
                sink.raise_notification(provider, &notification);
            }
            return;
        }

        if self.pending_text {
            sink.raise_event(provider, UIA_Text_TextChangedEventId);
        }
        if self.pending_selection {
            sink.raise_event(provider, UIA_Text_TextSelectionChangedEventId);
        }
        if self.pending_active_text_position {
            sink.raise_active_text_position_changed(provider);
        }
        if let Some(notification) = &self.pending_notification {
            sink.raise_notification(provider, notification);
        }

        self.pending_text = false;
        self.pending_selection = false;
        self.pending_active_text_position = false;
        self.pending_notification = None;
        self.last_emit = now;
        self.has_emitted = true;
    }

    fn record_notification(&mut self, notification: Option<String>) {
        let Some(notification) = notification else {
            return;
        };

        if notification.trim().is_empty() {
            return;
        }

        match &mut self.pending_notification {
            Some(pending) => {
                pending.push('\n');
                pending.push_str(&notification);
                truncate_notification(pending);
                trace_uia(&format!("throttle.notification_pending combined len={}", pending.len()));
            },
            None => {
                self.pending_notification = Some(truncated_notification(notification));
                trace_uia("throttle.notification_pending new");
            },
        }
    }
}

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
                || id == UIA_IsKeyboardFocusablePropertyId
                || id == UIA_IsTextPatternAvailablePropertyId
                || id == UIA_IsTextPattern2AvailablePropertyId =>
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
                || id == UIA_IsKeyboardFocusablePropertyId
                || id == UIA_IsTextPatternAvailablePropertyId
                || id == UIA_IsTextPattern2AvailablePropertyId =>
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
    last_snapshot: Mutex<Option<PublishedSnapshotState>>,
    event_throttle: Mutex<UiaEventThrottle>,
}

impl WindowsAccessibility {
    /// Attach a UIA provider to a Win32 window.
    ///
    /// # Safety
    ///
    /// `hwnd` must be a valid Alacritty window handle owned by the current UI thread, and the
    /// returned attachment must be dropped before the window is destroyed.
    pub unsafe fn new(hwnd: HWND, name: impl Into<String>) -> Option<Self> {
        let name = name.into();
        trace_uia(&format!("accessibility.new hwnd={hwnd:p} name={name:?}"));
        let provider = RawProvider::allocate(hwnd, TerminalProvider::new(name));
        let ref_data = provider.as_ptr() as usize;
        let installed = unsafe {
            SetWindowSubclass(hwnd, Some(accessibility_subclass_proc), SUBCLASS_ID, ref_data)
        };

        if installed == 0 {
            trace_uia("accessibility.new SetWindowSubclass failed");
            unsafe {
                release(provider.as_ptr().cast());
            }
            None
        } else {
            trace_uia("accessibility.new SetWindowSubclass installed");
            Some(Self {
                hwnd,
                provider,
                last_snapshot: Mutex::new(None),
                event_throttle: Mutex::new(UiaEventThrottle::new(
                    UIA_EVENT_THROTTLE,
                    Instant::now(),
                )),
            })
        }
    }

    pub fn raw_provider(&self) -> *mut c_void {
        self.provider.as_ptr().cast()
    }

    pub fn update_snapshot<T: EventListener>(&self, term: &Term<T>, size_info: &SizeInfo) {
        let snapshot = VisibleTerminalSnapshot::from_term(term);
        let layout = self.layout_for_snapshot(&snapshot, size_info);
        let selection = snapshot.selection_offsets(term);
        let raw_cursor = snapshot.cursor();
        let cursor_visible = term.mode().contains(TermMode::SHOW_CURSOR);
        let cursor_offset =
            snapshot.offset_for_point(raw_cursor).unwrap_or_else(|| snapshot.text().len());
        let cursor_row_text = snapshot.row_text(raw_cursor.line).unwrap_or_default().to_owned();
        let (cursor, text_changed, selection_changed, active_text_position_changed, notification) =
            self.snapshot_changes(
                snapshot.text().to_owned(),
                raw_cursor,
                cursor_visible,
                cursor_offset,
                cursor_row_text,
                selection.clone(),
            );
        unsafe {
            let provider = &*self.provider.as_ptr();
            provider.set_terminal_state(snapshot, layout, selection, cursor);
        }
        self.record_and_flush_events(
            text_changed,
            selection_changed,
            active_text_position_changed,
            notification,
        );
    }

    fn snapshot_changes(
        &self,
        text: String,
        raw_cursor: Point<usize>,
        cursor_visible: bool,
        cursor_offset: usize,
        cursor_row_text: String,
        selection: Vec<(usize, usize)>,
    ) -> (Point<usize>, bool, bool, bool, Option<String>) {
        let mut last_snapshot =
            self.last_snapshot.lock().expect("accessibility state lock poisoned");
        let cursor = accessibility_cursor(
            raw_cursor,
            cursor_visible,
            &cursor_row_text,
            last_snapshot.as_ref().map(|snapshot| snapshot.cursor),
        );
        let cursor_suppressed = cursor != raw_cursor;
        let text_changed = last_snapshot.as_ref().is_none_or(|snapshot| snapshot.text != text);
        let cursor_changed =
            last_snapshot.as_ref().is_none_or(|snapshot| snapshot.cursor != cursor);
        let active_text_position_changed = cursor_changed;
        let selection_changed = published_selection_changed(
            last_snapshot.as_ref().map(|snapshot| snapshot.selection.as_slice()),
            &selection,
        );
        let notification = last_snapshot
            .as_ref()
            .and_then(|snapshot| output_notification_text(&snapshot.text, &text));
        if text_changed || cursor_changed || selection_changed {
            trace_uia(&format!(
                "snapshot.state text_changed={text_changed} cursor_changed={cursor_changed} \
                 selection_changed={selection_changed} cursor_visible={cursor_visible} \
                 cursor_suppressed={cursor_suppressed} raw_cursor_row={} raw_cursor_col={} \
                 cursor_row={} cursor_col={} cursor_offset={} row_text={cursor_row_text:?}",
                raw_cursor.line, raw_cursor.column.0, cursor.line, cursor.column.0, cursor_offset,
            ));
        }
        if let Some(notification) = &notification {
            trace_uia(&format!(
                "snapshot.notification_candidate len={} text={notification:?}",
                notification.len()
            ));
        }
        *last_snapshot = Some(PublishedSnapshotState { text, cursor, selection });
        (cursor, text_changed, selection_changed, active_text_position_changed, notification)
    }

    fn record_and_flush_events(
        &self,
        text_changed: bool,
        selection_changed: bool,
        active_text_position_changed: bool,
        notification: Option<String>,
    ) {
        let mut throttle = self.event_throttle.lock().expect("event throttle lock poisoned");
        throttle.record_snapshot_change(
            text_changed,
            selection_changed,
            active_text_position_changed,
            notification,
        );
        let mut sink = NativeUiaEventSink { provider: self.provider };
        throttle.flush_due(self.raw_provider(), Instant::now(), &mut sink);
    }

    fn layout_for_snapshot(
        &self,
        snapshot: &VisibleTerminalSnapshot,
        size_info: &SizeInfo,
    ) -> Option<TextProviderLayout> {
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        let mut origin = POINT { x: 0, y: 0 };
        unsafe {
            if GetClientRect(self.hwnd, &mut rect) == 0
                || ClientToScreen(self.hwnd, &mut origin) == 0
            {
                return None;
            }
        }

        Some(TextProviderLayout::new(
            f64::from(origin.x) + f64::from(size_info.padding_x()),
            f64::from(origin.y) + f64::from(size_info.padding_y()),
            f64::from(size_info.cell_width()),
            f64::from(size_info.cell_height()),
            snapshot.columns(),
            snapshot.screen_lines(),
        ))
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
    message == WM_GETOBJECT
        && (lparam == UiaRootObjectId as isize || lparam == OBJID_CLIENT as isize)
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
    advise_events: RawProviderAdviseEvents,
    ref_count: AtomicU32,
    hwnd: HWND,
    provider: TerminalProvider,
    text_provider: NonNull<RawTextProvider>,
    advised_event_listeners: AtomicU32,
    #[cfg(test)]
    drop_counter: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

impl RawProvider {
    fn new(hwnd: HWND, provider: TerminalProvider) -> Self {
        let text_provider =
            RawTextProvider::allocate_for_window(hwnd, ptr::null_mut(), String::new());
        Self {
            vtable: &RAW_PROVIDER_VTABLE,
            advise_events: RawProviderAdviseEvents::new(),
            ref_count: AtomicU32::new(1),
            hwnd,
            provider,
            text_provider,
            advised_event_listeners: AtomicU32::new(0),
            #[cfg(test)]
            drop_counter: None,
        }
    }

    fn allocate(hwnd: HWND, provider: TerminalProvider) -> NonNull<Self> {
        let mut provider = Box::new(Self::new(hwnd, provider));
        unsafe {
            provider.set_enclosing_provider();
        }
        provider.set_advise_events_owner();
        NonNull::from(Box::leak(provider))
    }

    #[cfg(test)]
    fn allocate_with_drop_counter(
        hwnd: HWND,
        provider: TerminalProvider,
        drop_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> NonNull<Self> {
        let mut provider = Box::new(Self::new(hwnd, provider));
        unsafe {
            provider.set_enclosing_provider();
        }
        provider.set_advise_events_owner();
        provider.drop_counter = Some(drop_counter);
        NonNull::from(Box::leak(provider))
    }

    fn as_raw(&self) -> *mut c_void {
        self as *const Self as *mut c_void
    }

    unsafe fn set_enclosing_provider(&mut self) {
        unsafe {
            (*self.text_provider.as_ptr()).enclosing_provider = self.as_raw();
        }
    }

    fn set_advise_events_owner(&mut self) {
        self.advise_events.owner = self.as_raw();
    }

    fn set_terminal_state(
        &self,
        snapshot: VisibleTerminalSnapshot,
        layout: Option<TextProviderLayout>,
        selection: Vec<(usize, usize)>,
        cursor: Point<usize>,
    ) {
        unsafe {
            (*self.text_provider.as_ptr()).set_terminal_state(snapshot, layout, selection, cursor);
        }
    }

    fn has_advised_event_listeners(&self) -> bool {
        self.advised_event_listeners.load(Ordering::Relaxed) > 0
    }

    unsafe fn caret_range(&self) -> *mut c_void {
        unsafe { (*self.text_provider.as_ptr()).caret_range() }
    }

    fn advise_event_added(&self) {
        self.advised_event_listeners.fetch_add(1, Ordering::Relaxed);
    }

    fn advise_event_removed(&self) {
        let mut current = self.advised_event_listeners.load(Ordering::Relaxed);
        while current > 0 {
            match self.advised_event_listeners.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }
}

#[cfg(test)]
impl Drop for RawProvider {
    fn drop(&mut self) {
        unsafe {
            (*self.text_provider.as_ptr()).disconnect_enclosing_provider();
            text_pattern::text_provider_release(self.text_provider.as_ptr().cast());
        }
        if let Some(drop_counter) = &self.drop_counter {
            drop_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(not(test))]
impl Drop for RawProvider {
    fn drop(&mut self) {
        unsafe {
            (*self.text_provider.as_ptr()).disconnect_enclosing_provider();
            text_pattern::text_provider_release(self.text_provider.as_ptr().cast());
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

#[repr(C)]
#[derive(Debug)]
struct RawProviderAdviseEvents {
    vtable: &'static RawProviderAdviseEventsVtable,
    owner: *mut c_void,
}

impl RawProviderAdviseEvents {
    fn new() -> Self {
        Self { vtable: &RAW_PROVIDER_ADVISE_EVENTS_VTABLE, owner: ptr::null_mut() }
    }

    fn as_raw(&self) -> *mut c_void {
        self as *const Self as *mut c_void
    }
}

#[repr(C)]
struct RawProviderAdviseEventsVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    advise_event_added: unsafe extern "system" fn(*mut c_void, i32, *mut SAFEARRAY) -> HRESULT,
    advise_event_removed: unsafe extern "system" fn(*mut c_void, i32, *mut SAFEARRAY) -> HRESULT,
}

impl std::fmt::Debug for RawProviderAdviseEventsVtable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RawProviderAdviseEventsVtable")
    }
}

static RAW_PROVIDER_ADVISE_EVENTS_VTABLE: RawProviderAdviseEventsVtable =
    RawProviderAdviseEventsVtable {
        query_interface: advise_events_query_interface,
        add_ref: advise_events_add_ref,
        release: advise_events_release,
        advise_event_added,
        advise_event_removed,
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
            trace_uia("provider.QueryInterface IRawElementProviderSimple");
            add_ref(this);
            *interface = this;
            S_OK
        } else if guid_eq(&*iid, &IID_IRAW_ELEMENT_PROVIDER_ADVISE_EVENTS) {
            trace_uia("provider.QueryInterface IRawElementProviderAdviseEvents");
            let provider = &*(this as *const RawProvider);
            add_ref(this);
            *interface = provider.advise_events.as_raw();
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

unsafe extern "system" fn advise_events_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> HRESULT {
    if this.is_null() {
        return E_POINTER;
    }

    let advise_events = unsafe { &*(this as *const RawProviderAdviseEvents) };
    unsafe { query_interface(advise_events.owner, iid, interface) }
}

unsafe extern "system" fn advise_events_add_ref(this: *mut c_void) -> u32 {
    let advise_events = unsafe { &*(this as *const RawProviderAdviseEvents) };
    unsafe { add_ref(advise_events.owner) }
}

unsafe extern "system" fn advise_events_release(this: *mut c_void) -> u32 {
    let advise_events = unsafe { &*(this as *const RawProviderAdviseEvents) };
    unsafe { release(advise_events.owner) }
}

pub(crate) unsafe fn add_ref_raw_provider(provider: *mut c_void) -> u32 {
    if provider.is_null() { 0 } else { unsafe { add_ref(provider) } }
}

pub(crate) unsafe fn release_raw_provider(provider: *mut c_void) -> u32 {
    if provider.is_null() { 0 } else { unsafe { release(provider) } }
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
    trace_uia("provider.ProviderOptions");
    S_OK
}

unsafe extern "system" fn get_pattern_provider(
    this: *mut c_void,
    pattern_id: i32,
    pattern_provider: *mut *mut c_void,
) -> HRESULT {
    if pattern_provider.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawProvider) };
    unsafe {
        if pattern_id == UIA_TextPatternId || pattern_id == UIA_TextPattern2Id {
            let pattern_name =
                if pattern_id == UIA_TextPattern2Id { "TextPattern2" } else { "TextPattern" };
            trace_uia(&format!("provider.GetPatternProvider {pattern_name}"));
            let text_provider = provider.text_provider.as_ptr().cast();
            ((*provider.text_provider.as_ptr()).vtable.add_ref)(text_provider);
            *pattern_provider = text_provider;
        } else {
            trace_uia(&format!("provider.GetPatternProvider unsupported {pattern_id}"));
            *pattern_provider = ptr::null_mut();
        }
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
            trace_uia(&format!("provider.GetPropertyValue {property_id} VT_I4"));
            variant.vt = VT_I4;
            variant.Anonymous.lVal = property_value;
        } else if let Some(property_value) = provider.provider.property_bstr(property_id) {
            trace_uia(&format!("provider.GetPropertyValue {property_id} VT_BSTR"));
            variant.vt = VT_BSTR;
            variant.Anonymous.bstrVal = string_to_bstr(&property_value);
        } else if let Some(property_value) = provider.provider.property_bool(property_id) {
            trace_uia(&format!("provider.GetPropertyValue {property_id} VT_BOOL"));
            variant.vt = VT_BOOL;
            variant.Anonymous.boolVal = if property_value { VARIANT_TRUE } else { 0 };
        } else {
            trace_uia(&format!("provider.GetPropertyValue {property_id} VT_EMPTY"));
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
    trace_uia("provider.HostRawElementProvider");
    unsafe { UiaHostProviderFromHwnd(raw_provider.hwnd, provider) }
}

unsafe extern "system" fn advise_event_added(
    this: *mut c_void,
    _event_id: i32,
    _property_ids: *mut SAFEARRAY,
) -> HRESULT {
    if this.is_null() {
        return E_POINTER;
    }

    let advise_events = unsafe { &*(this as *const RawProviderAdviseEvents) };
    if !advise_events.owner.is_null() {
        trace_uia(&format!("provider.AdviseEventAdded {_event_id}"));
        let provider = unsafe { &*(advise_events.owner as *const RawProvider) };
        provider.advise_event_added();
    }

    S_OK
}

unsafe extern "system" fn advise_event_removed(
    this: *mut c_void,
    _event_id: i32,
    _property_ids: *mut SAFEARRAY,
) -> HRESULT {
    if this.is_null() {
        return E_POINTER;
    }

    let advise_events = unsafe { &*(this as *const RawProviderAdviseEvents) };
    if !advise_events.owner.is_null() {
        trace_uia(&format!("provider.AdviseEventRemoved {_event_id}"));
        let provider = unsafe { &*(advise_events.owner as *const RawProvider) };
        provider.advise_event_removed();
    }

    S_OK
}

fn guid_eq(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

fn output_notification_text(previous: &str, current: &str) -> Option<String> {
    if let Some(lines) = inserted_lines(previous, current) {
        return notification_from_lines(lines);
    }

    if let Some(lines) = rows_replacing_blank_lines(previous, current) {
        return notification_from_lines(lines);
    }

    let inserted = inserted_text(previous, current)?;
    if !inserted.contains('\n') {
        return None;
    }

    notification_from_lines(inserted.lines().collect())
}

fn inserted_lines<'a>(previous: &str, current: &'a str) -> Option<Vec<&'a str>> {
    if previous == current {
        return None;
    }

    let previous_lines = previous.lines().collect::<Vec<_>>();
    let current_lines = current.lines().collect::<Vec<_>>();
    let max_overlap = previous_lines.len().min(current_lines.len());
    for overlap in (1..=max_overlap).rev() {
        if previous_lines[previous_lines.len() - overlap..] == current_lines[..overlap]
            && current_lines.len() > overlap
        {
            return Some(current_lines[overlap..].to_vec());
        }
    }

    None
}

fn rows_replacing_blank_lines<'a>(previous: &str, current: &'a str) -> Option<Vec<&'a str>> {
    let previous_lines = previous.lines().collect::<Vec<_>>();
    let current_lines = current.lines().collect::<Vec<_>>();
    if previous_lines.len() != current_lines.len() {
        return None;
    }

    let rows = previous_lines
        .into_iter()
        .zip(current_lines)
        .filter_map(|(previous, current)| {
            (previous.trim().is_empty() && !current.trim().is_empty()).then_some(current)
        })
        .collect::<Vec<_>>();

    (!rows.is_empty()).then_some(rows)
}

fn notification_from_lines(lines: Vec<&str>) -> Option<String> {
    let text = lines
        .into_iter()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    (!text.is_empty()).then(|| truncated_notification(text))
}

fn inserted_text<'a>(previous: &str, current: &'a str) -> Option<&'a str> {
    if previous == current {
        return None;
    }

    let prefix = common_prefix_boundary(previous, current);
    let previous_suffix = &previous[prefix..];
    let current_suffix = &current[prefix..];
    let suffix = common_suffix_boundary(previous_suffix, current_suffix);
    if current_suffix.len() <= suffix {
        return None;
    }

    Some(&current_suffix[..current_suffix.len() - suffix])
}

fn common_prefix_boundary(left: &str, right: &str) -> usize {
    let mut prefix = 0;
    for ((left_index, left_char), (right_index, right_char)) in
        left.char_indices().zip(right.char_indices())
    {
        if left_char != right_char {
            break;
        }
        prefix = left_index + left_char.len_utf8();
        debug_assert_eq!(prefix, right_index + right_char.len_utf8());
    }
    prefix
}

fn common_suffix_boundary(left: &str, right: &str) -> usize {
    let mut suffix = 0;
    for (left_char, right_char) in left.chars().rev().zip(right.chars().rev()) {
        if left_char != right_char {
            break;
        }
        suffix += left_char.len_utf8();
    }
    suffix
}

fn truncated_notification(text: String) -> String {
    let mut text = text;
    truncate_notification(&mut text);
    text
}

fn truncate_notification(text: &mut String) {
    while text.chars().count() > MAX_NOTIFICATION_CHARS {
        let next = text.chars().next().map_or(0, char::len_utf8);
        text.drain(..next);
    }
}

fn accessibility_cursor(
    raw_cursor: Point<usize>,
    cursor_visible: bool,
    cursor_row_text: &str,
    previous_cursor: Option<Point<usize>>,
) -> Point<usize> {
    let should_preserve_previous =
        !cursor_visible || is_codex_status_footer_cursor(raw_cursor, cursor_row_text);
    if should_preserve_previous {
        previous_cursor.unwrap_or(raw_cursor)
    } else {
        raw_cursor
    }
}

fn published_selection_changed(
    previous_selection: Option<&[(usize, usize)]>,
    selection: &[(usize, usize)],
) -> bool {
    previous_selection != Some(selection)
}

fn is_codex_status_footer_cursor(cursor: Point<usize>, row_text: &str) -> bool {
    let row_width = row_text.chars().count();
    let cursor_near_row_end = cursor.column.0.saturating_add(8) >= row_width;

    cursor_near_row_end
        && row_text.contains(" \u{00b7} ")
        && row_text.contains(" Context ")
        && row_text.contains(" left")
        && row_text.contains(" in ")
        && row_text.contains(" out")
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
    use windows_sys::Win32::System::Variant::{VT_BOOL, VT_BSTR, VT_I4, VariantClear};
    use windows_sys::Win32::UI::Accessibility::{
        ProviderOptions_ServerSideProvider, UIA_ControlTypePropertyId,
        UIA_IsContentElementPropertyId, UIA_IsControlElementPropertyId,
        UIA_IsKeyboardFocusablePropertyId, UIA_IsTextPattern2AvailablePropertyId,
        UIA_IsTextPatternAvailablePropertyId, UIA_NamePropertyId, UIA_TextControlTypeId,
        UIA_TextPattern2Id, UIA_TextPatternId, UiaRootObjectId,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{OBJID_CLIENT, WM_GETOBJECT};

    use super::{
        RawProvider, TerminalProvider, accessibility_cursor, hwnd_from_raw_window_handle,
        output_notification_text, release, should_handle_wm_getobject,
    };
    use alacritty_terminal::index::{Column, Point};

    #[test]
    fn windows_provider_exposes_terminal_properties() {
        let provider = TerminalProvider::new("Alacritty");

        assert_eq!(provider.provider_options(), ProviderOptions_ServerSideProvider);
        assert_eq!(provider.property_i4(UIA_ControlTypePropertyId), Some(UIA_TextControlTypeId));
        assert_eq!(provider.property_bstr(UIA_NamePropertyId).as_deref(), Some("Alacritty"));
        assert_eq!(provider.property_bool(UIA_IsControlElementPropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsContentElementPropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsKeyboardFocusablePropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsTextPatternAvailablePropertyId), Some(true));
        assert_eq!(provider.property_bool(UIA_IsTextPattern2AvailablePropertyId), Some(true));
        assert_eq!(provider.property_variant_type(UIA_ControlTypePropertyId), Some(VT_I4));
        assert_eq!(provider.property_variant_type(UIA_NamePropertyId), Some(VT_BSTR));
        assert_eq!(
            provider.property_variant_type(UIA_IsKeyboardFocusablePropertyId),
            Some(VT_BOOL)
        );
        assert_eq!(
            provider.property_variant_type(UIA_IsTextPattern2AvailablePropertyId),
            Some(VT_BOOL)
        );
    }

    #[test]
    fn output_notification_text_reports_inserted_terminal_rows() {
        let previous = "PS> echo hello";
        let current = "PS> echo hello\nhello\nPS> ";

        assert_eq!(output_notification_text(previous, current).as_deref(), Some("hello\nPS>"));
    }

    #[test]
    fn output_notification_text_reports_rows_inserted_before_blank_viewport_tail() {
        let previous = "PS> echo foobar\n\n\n";
        let current = "PS> echo foobar\nfoobar\nPS> \n\n\n";

        assert_eq!(output_notification_text(previous, current).as_deref(), Some("foobar\nPS>"));
    }

    #[test]
    fn output_notification_text_reports_single_output_row_replacing_blank_line() {
        let previous = "PS> echo foobar\n\n\n";
        let current = "PS> echo foobar\nfoobar\n\n";

        assert_eq!(output_notification_text(previous, current).as_deref(), Some("foobar"));
    }

    #[test]
    fn output_notification_text_ignores_single_line_typing() {
        assert_eq!(output_notification_text("PS> ech", "PS> echo"), None);
    }

    #[test]
    fn accessibility_cursor_tracks_visible_terminal_cursor() {
        let previous = Point::new(1, Column(5));
        let raw = Point::new(3, Column(9));

        assert_eq!(accessibility_cursor(raw, true, "shell prompt", Some(previous)), raw);
    }

    #[test]
    fn accessibility_cursor_preserves_previous_caret_while_terminal_cursor_is_hidden() {
        let previous = Point::new(1, Column(5));
        let hidden_repaint_cursor = Point::new(3, Column(90));

        assert_eq!(
            accessibility_cursor(hidden_repaint_cursor, false, "shell prompt", Some(previous)),
            previous
        );
    }

    #[test]
    fn accessibility_cursor_uses_raw_cursor_without_previous_caret() {
        let raw = Point::new(3, Column(9));

        assert_eq!(accessibility_cursor(raw, false, "shell prompt", None), raw);
    }

    #[test]
    fn accessibility_cursor_preserves_previous_caret_for_codex_status_footer() {
        let previous = Point::new(19, Column(13));
        let footer_cursor = Point::new(22, Column(102));
        let footer = "  gpt-5.5 medium \u{00b7} F:\\projects\\MaxLogic\\alacritty \
                      \u{00b7} Context 100% left \u{00b7} weekly 55% left \
                      \u{00b7} 0 in \u{00b7} 0 out";

        assert_eq!(accessibility_cursor(footer_cursor, true, footer, Some(previous)), previous);
    }

    #[test]
    fn accessibility_cursor_keeps_visible_cursor_on_normal_text_near_row_end() {
        let previous = Point::new(19, Column(13));
        let raw = Point::new(20, Column(31));
        let input = "  test6 test7 test8 test9 test10";

        assert_eq!(accessibility_cursor(raw, true, input, Some(previous)), raw);
    }

    #[test]
    fn caret_only_move_does_not_publish_selection_change() {
        assert!(!super::published_selection_changed(Some(&[]), &[]));
        assert!(super::published_selection_changed(Some(&[]), &[(1, 4)]));
    }

    #[test]
    fn output_notification_text_handles_scrolled_viewport() {
        let previous = "first\nsecond\nthird";
        let current = "second\nthird\nfourth";

        assert_eq!(output_notification_text(previous, current).as_deref(), Some("fourth"));
    }

    #[test]
    fn wm_getobject_handler_matches_uia_client_object_request() {
        assert!(should_handle_wm_getobject(WM_GETOBJECT, UiaRootObjectId as isize));
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

    #[test]
    fn raw_provider_returns_text_pattern_provider() {
        let provider = RawProvider::new(42 as _, TerminalProvider::new("Alacritty"));
        let raw_provider = provider.as_raw();

        unsafe {
            let mut pattern_provider = std::ptr::null_mut();
            assert_eq!(
                (provider.vtable.get_pattern_provider)(
                    raw_provider,
                    UIA_TextPatternId,
                    &mut pattern_provider,
                ),
                0,
            );
            assert!(!pattern_provider.is_null());

            let text_provider =
                pattern_provider as *mut crate::accessibility::text_pattern::RawTextProvider;
            (((*text_provider).vtable).release)(pattern_provider);
        }
    }

    #[test]
    fn raw_provider_returns_text_pattern2_provider() {
        let provider = RawProvider::new(42 as _, TerminalProvider::new("Alacritty"));
        let raw_provider = provider.as_raw();

        unsafe {
            let mut pattern_provider = std::ptr::null_mut();
            assert_eq!(
                (provider.vtable.get_pattern_provider)(
                    raw_provider,
                    UIA_TextPattern2Id,
                    &mut pattern_provider,
                ),
                0,
            );
            assert!(!pattern_provider.is_null());

            let text_provider =
                pattern_provider as *mut crate::accessibility::text_pattern::RawTextProvider;
            (((*text_provider).vtable).release)(pattern_provider);
        }
    }

    #[test]
    fn text_range_enclosing_element_returns_addrefed_provider() {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = RawProvider::allocate_with_drop_counter(
            42 as _,
            TerminalProvider::new("Alacritty"),
            std::sync::Arc::clone(&dropped),
        );
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut pattern_provider = std::ptr::null_mut();
            assert_eq!(
                (vtable.get_pattern_provider)(
                    raw_provider,
                    UIA_TextPatternId,
                    &mut pattern_provider
                ),
                0,
            );

            let text_provider =
                pattern_provider as *mut crate::accessibility::text_pattern::RawTextProvider;
            let text_vtable = (*text_provider).vtable;
            let mut range = std::ptr::null_mut();
            assert_eq!((text_vtable.document_range)(pattern_provider, &mut range), 0);

            let range_provider = range as *mut crate::accessibility::text_pattern::RawTextRange;
            let range_vtable = (*range_provider).vtable;
            let mut enclosing = std::ptr::null_mut();
            assert_eq!((range_vtable.get_enclosing_element)(range, &mut enclosing), 0);
            assert_eq!(enclosing, raw_provider);

            (release)(enclosing);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 0);

            (range_vtable.release)(range);
            (text_vtable.release)(pattern_provider);
            assert_eq!((release)(raw_provider), 0);
        }

        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn text_provider_disconnects_enclosing_provider_when_window_provider_drops() {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = RawProvider::allocate_with_drop_counter(
            42 as _,
            TerminalProvider::new("Alacritty"),
            std::sync::Arc::clone(&dropped),
        );
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut pattern_provider = std::ptr::null_mut();
            assert_eq!(
                (vtable.get_pattern_provider)(
                    raw_provider,
                    UIA_TextPatternId,
                    &mut pattern_provider
                ),
                0,
            );

            assert_eq!((release)(raw_provider), 0);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);

            let text_provider =
                pattern_provider as *mut crate::accessibility::text_pattern::RawTextProvider;
            let text_vtable = (*text_provider).vtable;
            let mut range = std::ptr::null_mut();
            assert_eq!((text_vtable.document_range)(pattern_provider, &mut range), 0);

            let range_provider = range as *mut crate::accessibility::text_pattern::RawTextRange;
            let range_vtable = (*range_provider).vtable;
            let mut enclosing = std::ptr::null_mut();
            assert_eq!((range_vtable.get_enclosing_element)(range, &mut enclosing), 0);
            assert!(enclosing.is_null());

            (range_vtable.release)(range);
            (text_vtable.release)(pattern_provider);
        }
    }
}
