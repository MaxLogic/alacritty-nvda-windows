//! Windows UI Automation TextPattern provider.

use std::ffi::c_void;
use std::ptr;
use std::ptr::NonNull;
use std::sync::RwLock;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use alacritty_terminal::index::Point;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Foundation::{BOOL, E_FAIL, HWND, S_OK, SysAllocStringLen};
use windows_sys::Win32::System::Com::SAFEARRAY;
use windows_sys::Win32::System::Ole::{
    SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement, SafeArraySetIID,
};
use windows_sys::Win32::System::Variant::{
    VARENUM, VARIANT, VT_EMPTY, VT_R8, VT_UNKNOWN, VariantInit,
};
use windows_sys::Win32::UI::Accessibility::{
    SupportedTextSelection, SupportedTextSelection_Multiple, TextPatternRangeEndpoint,
    TextPatternRangeEndpoint_Start, TextUnit, TextUnit_Character, TextUnit_Document, TextUnit_Line,
    TextUnit_Paragraph, TextUnit_Word, UIA_TEXTATTRIBUTE_ID, UiaPoint,
};
use windows_sys::core::{BSTR, GUID, HRESULT};

use crate::accessibility::snapshot::VisibleTerminalSnapshot;
use crate::accessibility::windows_provider::{add_ref_raw_provider, release_raw_provider};

const E_INVALIDARG: HRESULT = 0x8007_0057u32 as i32;
const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
const E_NOTIMPL: HRESULT = 0x8000_4001u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_ITEXT_PROVIDER: GUID = GUID::from_u128(0x3589c92c_63f3_4367_99bb_ada653b77cf2);
const IID_ITEXT_PROVIDER2: GUID = GUID::from_u128(0x0dc5e6ed_3e16_4bf1_8f9a_a979878bc195);
const IID_ITEXT_RANGE_PROVIDER: GUID = GUID::from_u128(0x5347ad7b_c355_46f8_aff5_909033582f63);
static NEXT_TEXT_PROVIDER_GENERATION: AtomicU64 = AtomicU64::new(1);

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

/// UIA TextPattern provider backed by the latest visible terminal text.
#[repr(C)]
pub struct RawTextProvider {
    pub(crate) vtable: &'static RawTextProviderVtable,
    ref_count: AtomicU32,
    state: RwLock<TextProviderState>,
    hwnd: HWND,
    enclosing_provider: AtomicPtr<c_void>,
    generation: u64,
}

#[derive(Clone, Debug)]
struct TextProviderState {
    text: String,
    terminal: Option<VisibleTerminalSnapshot>,
    layout: Option<TextProviderLayout>,
    selection: Vec<(usize, usize)>,
    cursor: Option<Point<usize>>,
    focused: bool,
}

impl TextProviderState {
    fn from_text(text: String) -> Self {
        Self {
            text,
            terminal: None,
            layout: None,
            selection: Vec::new(),
            cursor: None,
            focused: false,
        }
    }

    fn from_terminal(
        snapshot: VisibleTerminalSnapshot,
        layout: Option<TextProviderLayout>,
        selection: Vec<(usize, usize)>,
        cursor: Point<usize>,
        focused: bool,
    ) -> Self {
        Self {
            text: snapshot.text().to_owned(),
            terminal: Some(snapshot),
            layout,
            selection,
            cursor: Some(cursor),
            focused,
        }
    }

    fn offset_for_point(&self, row: usize, column: usize) -> usize {
        if let Some(snapshot) = &self.terminal {
            return snapshot
                .offset_for_point(alacritty_terminal::index::Point::new(
                    row,
                    alacritty_terminal::index::Column(column),
                ))
                .unwrap_or(self.text.len());
        }

        offset_for_line_column(&self.text, row, column)
    }

    fn cursor_offset(&self) -> usize {
        let Some(snapshot) = &self.terminal else {
            return self.text.len();
        };

        let cursor = self.cursor.unwrap_or_else(|| snapshot.cursor());
        snapshot.offset_for_point(cursor).unwrap_or(self.text.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextProviderLayout {
    origin_x: f64,
    origin_y: f64,
    cell_width: f64,
    cell_height: f64,
    columns: usize,
    rows: usize,
}

impl TextProviderLayout {
    pub(crate) fn new(
        origin_x: f64,
        origin_y: f64,
        cell_width: f64,
        cell_height: f64,
        columns: usize,
        rows: usize,
    ) -> Self {
        Self { origin_x, origin_y, cell_width, cell_height, columns, rows }
    }

    fn point_to_cell(self, point: UiaPoint) -> Option<(usize, usize)> {
        if self.cell_width <= 0.0 || self.cell_height <= 0.0 || self.columns == 0 || self.rows == 0
        {
            return None;
        }

        let column = ((point.x - self.origin_x) / self.cell_width).floor() as isize;
        let row = ((point.y - self.origin_y) / self.cell_height).floor() as isize;
        let column = column.clamp(0, self.columns.saturating_sub(1) as isize) as usize;
        let row = row.clamp(0, self.rows.saturating_sub(1) as isize) as usize;

        Some((row, column))
    }
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
            state: RwLock::new(TextProviderState::from_text(text)),
            hwnd,
            enclosing_provider: AtomicPtr::new(enclosing_provider),
            generation: NEXT_TEXT_PROVIDER_GENERATION.fetch_add(1, Ordering::Relaxed),
        });
        NonNull::from(Box::leak(provider))
    }

    pub(crate) fn set_terminal_state(
        &self,
        snapshot: VisibleTerminalSnapshot,
        layout: Option<TextProviderLayout>,
        selection: Vec<(usize, usize)>,
        cursor: Point<usize>,
        focused: bool,
    ) {
        *self.state.write().expect("text provider lock poisoned") =
            TextProviderState::from_terminal(snapshot, layout, selection, cursor, focused);
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        self.state.write().expect("text provider lock poisoned").focused = focused;
    }

    #[cfg(test)]
    pub(crate) fn set_test_layout(
        &self,
        origin_x: f64,
        origin_y: f64,
        cell_width: f64,
        cell_height: f64,
        columns: usize,
        rows: usize,
    ) {
        let text = self.text();
        let snapshot = VisibleTerminalSnapshot::from_text_for_tests(&text, columns, rows);
        self.set_terminal_state(
            snapshot,
            Some(TextProviderLayout::new(
                origin_x,
                origin_y,
                cell_width,
                cell_height,
                columns,
                rows,
            )),
            Vec::new(),
            alacritty_terminal::index::Point::new(0, alacritty_terminal::index::Column(0)),
            true,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_test_selection(&self, start: usize, end: usize) {
        let mut state = self.state.write().expect("text provider lock poisoned");
        state.selection = vec![(start, end)];
    }

    #[cfg(test)]
    pub(crate) fn set_test_selections(&self, ranges: Vec<(usize, usize)>) {
        let mut state = self.state.write().expect("text provider lock poisoned");
        state.selection = ranges;
    }

    pub(crate) fn set_enclosing_provider(&self, provider: *mut c_void) {
        self.enclosing_provider.store(provider, Ordering::Release);
    }

    pub(crate) fn disconnect_enclosing_provider(&self) {
        self.set_enclosing_provider(ptr::null_mut());
    }

    fn enclosing_provider(&self) -> *mut c_void {
        // The enclosing provider pointer is cleared when the owner disconnects. Atomic access
        // avoids racing UIA callbacks that still hold an AddRef'd text provider.
        self.enclosing_provider.load(Ordering::Acquire)
    }

    fn text(&self) -> String {
        self.state.read().expect("text provider lock poisoned").text.clone()
    }

    fn text_and_selection(&self) -> (String, Vec<(usize, usize)>) {
        let state = self.state.read().expect("text provider lock poisoned");
        let text = state.text.clone();
        let selection = if state.selection.is_empty() {
            let caret = clamp_to_boundary(&text, state.cursor_offset());
            vec![(caret, caret)]
        } else {
            state.selection.clone()
        };
        (text, selection)
    }

    fn range_from_point(&self, point: UiaPoint) -> (String, usize) {
        let state = self.state.read().expect("text provider lock poisoned").clone();
        let offset = state
            .layout
            .and_then(|layout| layout.point_to_cell(point))
            .map_or(0, |(row, column)| state.offset_for_point(row, column));

        (state.text, offset)
    }

    pub(crate) fn caret_range(&self) -> *mut c_void {
        let state = self.state.read().expect("text provider lock poisoned");
        let offset = state.cursor_offset();
        trace_uia(&format!("text_provider.GetCaretRange offset={offset}"));
        RawTextRange::allocate(
            state.text.clone(),
            offset,
            offset,
            self.enclosing_provider(),
            self.generation,
        )
        .as_ptr()
        .cast()
    }
}

#[repr(C)]
pub(crate) struct RawTextProviderVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub(crate) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(crate) release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(crate) get_selection:
        unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    get_visible_ranges: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    range_from_child:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    pub(crate) range_from_point:
        unsafe extern "system" fn(*mut c_void, UiaPoint, *mut *mut c_void) -> HRESULT,
    pub(crate) document_range: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub(crate) supported_text_selection:
        unsafe extern "system" fn(*mut c_void, *mut SupportedTextSelection) -> HRESULT,
    range_from_annotation:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    pub(crate) get_caret_range:
        unsafe extern "system" fn(*mut c_void, *mut BOOL, *mut *mut c_void) -> HRESULT,
}

static RAW_TEXT_PROVIDER_VTABLE: RawTextProviderVtable = RawTextProviderVtable {
    query_interface: text_provider_query_interface_ffi,
    add_ref: text_provider_add_ref_ffi,
    release: text_provider_release_ffi,
    get_selection: text_provider_get_selection_ffi,
    get_visible_ranges: text_provider_get_visible_ranges_ffi,
    range_from_child: text_provider_range_from_child_ffi,
    range_from_point: text_provider_range_from_point_ffi,
    document_range: text_provider_document_range_ffi,
    supported_text_selection: text_provider_supported_text_selection_ffi,
    range_from_annotation: text_provider_range_from_annotation_ffi,
    get_caret_range: text_provider_get_caret_range_ffi,
};

unsafe fn text_provider_query_interface(
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
            trace_uia("text_provider.QueryInterface ITextProvider");
            // SAFETY: A successful COM QueryInterface must return an AddRef'd interface pointer.
            text_provider_add_ref(this);
            *interface = this;
            S_OK
        } else if guid_eq(&*iid, &IID_ITEXT_PROVIDER2) {
            trace_uia("text_provider.QueryInterface ITextProvider2");
            // SAFETY: ITextProvider2 is implemented by the same allocation and COM identity.
            text_provider_add_ref(this);
            *interface = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
}

unsafe fn text_provider_add_ref(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawTextProvider) };
    provider.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) unsafe fn text_provider_release(this: *mut c_void) -> u32 {
    let provider = unsafe { &*(this as *const RawTextProvider) };
    let previous = provider.ref_count.fetch_sub(1, Ordering::Release);
    let remaining = previous.saturating_sub(1);

    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe {
            // SAFETY: The text provider COM refcount reached zero, so this is the final release.
            drop(Box::from_raw(this as *mut RawTextProvider));
        }
    }

    remaining
}

unsafe fn text_provider_get_selection(this: *mut c_void, ranges: *mut *mut SAFEARRAY) -> HRESULT {
    if ranges.is_null() {
        return E_POINTER;
    }

    unsafe { *ranges = ptr::null_mut() };
    trace_uia("text_provider.GetSelection");
    let provider = unsafe { &*(this as *const RawTextProvider) };
    let (text, selection) = provider.text_and_selection();

    let array = unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, selection.len() as u32) };
    if array.is_null() {
        return E_FAIL;
    }
    let _ = unsafe { SafeArraySetIID(array, &IID_IUNKNOWN) };

    for (index, (start, end)) in selection.into_iter().enumerate() {
        let range = RawTextRange::allocate(
            text.clone(),
            clamp_to_boundary(&text, start),
            clamp_to_boundary(&text, end),
            provider.enclosing_provider(),
            provider.generation,
        );
        let index = index as i32;
        // SAFETY: The SAFEARRAY stores VT_UNKNOWN interface pointers. `SafeArrayPutElement`
        // AddRefs the supplied COM object, so the local range reference is released below.
        if unsafe { SafeArrayPutElement(array, &index, range.as_ptr().cast::<c_void>()) } != S_OK {
            unsafe {
                text_range_release(range.as_ptr().cast());
                SafeArrayDestroy(array);
            }
            return E_FAIL;
        }
        unsafe {
            text_range_release(range.as_ptr().cast());
        }
    }

    unsafe { *ranges = array };

    S_OK
}

unsafe fn text_provider_get_visible_ranges(
    this: *mut c_void,
    ranges: *mut *mut SAFEARRAY,
) -> HRESULT {
    if ranges.is_null() {
        return E_POINTER;
    }

    unsafe { *ranges = ptr::null_mut() };
    trace_uia("text_provider.GetVisibleRanges");
    let provider = unsafe { &*(this as *const RawTextProvider) };
    let text = provider.text();
    let end = text.len();
    let range =
        RawTextRange::allocate(text, 0, end, provider.enclosing_provider(), provider.generation);
    // SAFETY: `single_unknown_safearray` stores the AddRef'd COM range in a VT_UNKNOWN SAFEARRAY;
    // the local reference is released immediately after the array takes its reference.
    let array = unsafe { single_unknown_safearray(range.as_ptr().cast()) };
    unsafe {
        text_range_release(range.as_ptr().cast());
        *ranges = array;
    }

    if array.is_null() { E_FAIL } else { S_OK }
}

unsafe fn text_provider_range_from_child(
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

unsafe fn text_provider_range_from_point(
    this: *mut c_void,
    point: UiaPoint,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawTextProvider) };
    let (text, offset) = provider.range_from_point(point);
    trace_uia(&format!("text_provider.RangeFromPoint offset={offset}"));
    let text_range = RawTextRange::allocate(
        text,
        offset,
        offset,
        provider.enclosing_provider(),
        provider.generation,
    );
    unsafe { *range = text_range.as_ptr().cast() };
    S_OK
}

unsafe fn text_provider_document_range(this: *mut c_void, range: *mut *mut c_void) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawTextProvider) };
    let text = provider.text();
    trace_uia(&format!("text_provider.DocumentRange len={}", text.len()));
    let text_range = RawTextRange::allocate(
        text.clone(),
        0,
        text.len(),
        provider.enclosing_provider(),
        provider.generation,
    );
    unsafe { *range = text_range.as_ptr().cast() };
    S_OK
}

unsafe fn text_provider_supported_text_selection(
    _this: *mut c_void,
    selection: *mut SupportedTextSelection,
) -> HRESULT {
    if selection.is_null() {
        return E_POINTER;
    }

    unsafe { *selection = SupportedTextSelection_Multiple };
    trace_uia("text_provider.SupportedTextSelection multiple");
    S_OK
}

unsafe fn text_provider_range_from_annotation(
    _this: *mut c_void,
    _annotation: *mut c_void,
    range: *mut *mut c_void,
) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    unsafe { *range = ptr::null_mut() };
    E_INVALIDARG
}

unsafe fn text_provider_get_caret_range(
    this: *mut c_void,
    is_active: *mut BOOL,
    range: *mut *mut c_void,
) -> HRESULT {
    if is_active.is_null() || range.is_null() {
        return E_POINTER;
    }

    let provider = unsafe { &*(this as *const RawTextProvider) };
    unsafe {
        *is_active = provider.state.read().expect("text provider lock poisoned").focused as BOOL;
        *range = provider.caret_range();
    }
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
    provider_generation: u64,
}

impl RawTextRange {
    fn allocate(
        text: String,
        start: usize,
        end: usize,
        enclosing_provider: *mut c_void,
        provider_generation: u64,
    ) -> NonNull<Self> {
        let (start, end) = normalized_range_bounds(&text, start, end);
        unsafe {
            // SAFETY: Each text range keeps the enclosing provider alive while clients can ask
            // for its enclosing element.
            add_ref_raw_provider(enclosing_provider);
        }
        let range = Box::new(Self {
            vtable: &RAW_TEXT_RANGE_VTABLE,
            ref_count: AtomicU32::new(1),
            text,
            start,
            end,
            enclosing_provider,
            provider_generation,
        });
        NonNull::from(Box::leak(range))
    }

    fn selected_text(&self, max_length: i32) -> String {
        let (start, end) = normalized_range_bounds(&self.text, self.start, self.end);
        let text = self.text.get(start..end).unwrap_or_default();
        if max_length < 0 {
            text.to_owned()
        } else {
            text.chars().take(max_length as usize).collect()
        }
    }

    fn endpoint_offset(&self, endpoint: TextPatternRangeEndpoint) -> usize {
        if endpoint == TextPatternRangeEndpoint_Start { self.start } else { self.end }
    }

    fn set_start(&mut self, start: usize) {
        self.start = clamp_to_boundary(&self.text, start);
        if self.start > self.end {
            self.end = self.start;
        }
    }

    fn set_end(&mut self, end: usize) {
        self.end = clamp_to_boundary(&self.text, end);
        if self.end < self.start {
            self.start = self.end;
        }
    }

    fn set_range(&mut self, start: usize, end: usize) {
        (self.start, self.end) = normalized_range_bounds(&self.text, start, end);
    }
}

impl Drop for RawTextRange {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: This balances the AddRef in `RawTextRange::allocate`.
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
    pub(crate) compare_endpoints: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut i32,
    ) -> HRESULT,
    pub(crate) expand_to_enclosing_unit:
        unsafe extern "system" fn(*mut c_void, TextUnit) -> HRESULT,
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
    pub(crate) move_range:
        unsafe extern "system" fn(*mut c_void, TextUnit, i32, *mut i32) -> HRESULT,
    pub(crate) move_endpoint_by_unit: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        TextUnit,
        i32,
        *mut i32,
    ) -> HRESULT,
    pub(crate) move_endpoint_by_range: unsafe extern "system" fn(
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
    query_interface: text_range_query_interface_ffi,
    add_ref: text_range_add_ref_ffi,
    release: text_range_release_ffi,
    clone: text_range_clone_ffi,
    compare: text_range_compare_ffi,
    compare_endpoints: text_range_compare_endpoints_ffi,
    expand_to_enclosing_unit: text_range_expand_to_enclosing_unit_ffi,
    find_attribute: text_range_find_attribute_ffi,
    find_text: text_range_find_text_ffi,
    get_attribute_value: text_range_get_attribute_value_ffi,
    get_bounding_rectangles: text_range_get_bounding_rectangles_ffi,
    get_enclosing_element: text_range_get_enclosing_element_ffi,
    get_text: text_range_get_text_ffi,
    move_range: text_range_move_ffi,
    move_endpoint_by_unit: text_range_move_endpoint_by_unit_ffi,
    move_endpoint_by_range: text_range_move_endpoint_by_range_ffi,
    select: text_range_select_ffi,
    add_to_selection: text_range_add_to_selection_ffi,
    remove_from_selection: text_range_remove_from_selection_ffi,
    scroll_into_view: text_range_scroll_into_view_ffi,
    get_children: text_range_get_children_ffi,
};

unsafe fn text_range_query_interface(
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
            trace_uia("text_range.QueryInterface ITextRangeProvider");
            // SAFETY: A successful COM QueryInterface must return an AddRef'd interface pointer.
            text_range_add_ref(this);
            *interface = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
}

unsafe fn text_range_add_ref(this: *mut c_void) -> u32 {
    let range = unsafe { &*(this as *const RawTextRange) };
    range.ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) unsafe fn text_range_release(this: *mut c_void) -> u32 {
    let range = unsafe { &*(this as *const RawTextRange) };
    let previous = range.ref_count.fetch_sub(1, Ordering::Release);
    let remaining = previous.saturating_sub(1);

    if remaining == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        unsafe {
            // SAFETY: The text range COM refcount reached zero, so this is the final release.
            drop(Box::from_raw(this as *mut RawTextRange));
        }
    }

    remaining
}

pub(crate) unsafe fn release_text_range(range: *mut c_void) -> u32 {
    if range.is_null() { 0 } else { unsafe { text_range_release(range) } }
}

unsafe fn text_range_clone(this: *mut c_void, range: *mut *mut c_void) -> HRESULT {
    if range.is_null() {
        return E_POINTER;
    }

    let source = unsafe { &*(this as *const RawTextRange) };
    let clone = RawTextRange::allocate(
        source.text.clone(),
        source.start,
        source.end,
        source.enclosing_provider,
        source.provider_generation,
    );
    unsafe { *range = clone.as_ptr().cast() };
    S_OK
}

unsafe fn text_range_compare(this: *mut c_void, other: *mut c_void, equal: *mut BOOL) -> HRESULT {
    if equal.is_null() {
        return E_POINTER;
    }
    if other.is_null() {
        unsafe { *equal = 0 };
        return S_OK;
    }

    let left = unsafe { &*(this as *const RawTextRange) };
    unsafe { *equal = 0 };
    let right = match unsafe { compatible_text_range(other, left.provider_generation) } {
        Ok(right) => unsafe { right.as_ref() },
        Err(error) => return error,
    };
    unsafe {
        *equal =
            (left.start == right.start && left.end == right.end && left.text == right.text) as BOOL;
    }
    S_OK
}

unsafe fn text_range_compare_endpoints(
    this: *mut c_void,
    endpoint: TextPatternRangeEndpoint,
    target_range: *mut c_void,
    target_endpoint: TextPatternRangeEndpoint,
    comparison: *mut i32,
) -> HRESULT {
    if comparison.is_null() {
        return E_POINTER;
    }

    if target_range.is_null() {
        return E_INVALIDARG;
    }

    let source = unsafe { &*(this as *const RawTextRange) };
    let target = match unsafe { compatible_text_range(target_range, source.provider_generation) } {
        Ok(target) => unsafe { target.as_ref() },
        Err(error) => return error,
    };
    let source_offset = source.endpoint_offset(endpoint);
    let target_offset = target.endpoint_offset(target_endpoint);
    unsafe { *comparison = source_offset.cmp(&target_offset) as i32 };
    S_OK
}

unsafe fn text_range_expand_to_enclosing_unit(this: *mut c_void, unit: TextUnit) -> HRESULT {
    let range = unsafe { &mut *(this as *mut RawTextRange) };
    trace_uia(&format!("text_range.ExpandToEnclosingUnit {unit}"));
    if unit == TextUnit_Character {
        let start = previous_character_boundary(&range.text, range.start);
        range.set_range(start, next_char_boundary(&range.text, start));
    } else if unit == TextUnit_Word {
        let (start, end) = if range.start == range.end {
            caret_word_bounds(&range.text, range.start)
        } else {
            word_bounds(&range.text, range.start)
        };
        range.set_range(start, end);
    } else if is_terminal_line_unit(unit) {
        let (start, end) = line_bounds(&range.text, range.start);
        range.set_range(start, end);
    } else if unit == TextUnit_Document {
        range.set_range(0, range.text.len());
    }
    S_OK
}

unsafe fn text_range_find_attribute(
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

unsafe fn text_range_find_text(
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

unsafe fn text_range_get_attribute_value(
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
    trace_uia(&format!("text_range.GetAttributeValue {_attribute_id} VT_EMPTY"));
    S_OK
}

unsafe fn text_range_get_bounding_rectangles(
    _this: *mut c_void,
    rectangles: *mut *mut SAFEARRAY,
) -> HRESULT {
    if rectangles.is_null() {
        return E_POINTER;
    }

    unsafe { *rectangles = empty_array(VT_R8) };
    trace_uia("text_range.GetBoundingRectangles VT_R8 empty");
    S_OK
}

unsafe fn text_range_get_enclosing_element(
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
    trace_uia("text_range.GetEnclosingElement");
    S_OK
}

unsafe fn text_range_get_text(this: *mut c_void, max_length: i32, text: *mut BSTR) -> HRESULT {
    if text.is_null() {
        return E_POINTER;
    }

    let range = unsafe { &*(this as *const RawTextRange) };
    let selected_text = range.selected_text(max_length);
    trace_uia(&format!("text_range.GetText max={max_length} len={}", selected_text.len()));
    unsafe { *text = string_to_bstr(&selected_text) };
    S_OK
}

unsafe fn text_range_move(
    this: *mut c_void,
    unit: TextUnit,
    count: i32,
    moved: *mut i32,
) -> HRESULT {
    if moved.is_null() {
        return E_POINTER;
    }

    if count == 0 {
        unsafe { *moved = 0 };
        return S_OK;
    }

    let range = unsafe { &mut *(this as *mut RawTextRange) };
    let degenerate = range.start == range.end;
    let width = range.end.saturating_sub(range.start);
    let start = normalized_unit_start(&range.text, range.start, unit);
    let (mut new_start, mut actual) = move_offset_by_unit(&range.text, start, unit, count);
    if unit == TextUnit_Character && !degenerate && new_start == range.text.len() && new_start > 0 {
        let (clipped_start, adjustment) = move_offset_by_unit(&range.text, new_start, unit, -1);
        new_start = clipped_start;
        actual += adjustment;
    }
    if unit == TextUnit_Character && !degenerate {
        range.set_range(new_start, next_char_boundary(&range.text, new_start));
    } else if unit == TextUnit_Word {
        let (_, end) = word_bounds(&range.text, new_start);
        range.set_range(new_start, end);
    } else if is_terminal_line_unit(unit) {
        let (_, end) = line_bounds(&range.text, new_start);
        range.set_range(new_start, end);
    } else {
        range.set_range(new_start, new_start.saturating_add(width));
    }
    unsafe { *moved = actual };
    trace_uia(&format!("text_range.Move unit={unit} count={count} moved={actual}"));
    S_OK
}

unsafe fn text_range_move_endpoint_by_unit(
    this: *mut c_void,
    endpoint: TextPatternRangeEndpoint,
    unit: TextUnit,
    count: i32,
    moved: *mut i32,
) -> HRESULT {
    if moved.is_null() {
        return E_POINTER;
    }

    if count == 0 {
        unsafe { *moved = 0 };
        return S_OK;
    }

    let range = unsafe { &mut *(this as *mut RawTextRange) };
    let offset = normalized_unit_start(&range.text, range.endpoint_offset(endpoint), unit);
    let (new_offset, actual) = move_offset_by_unit(&range.text, offset, unit, count);

    if endpoint == TextPatternRangeEndpoint_Start {
        range.set_start(new_offset);
    } else {
        range.set_end(new_offset);
    }

    unsafe { *moved = actual };
    trace_uia(&format!(
        "text_range.MoveEndpointByUnit endpoint={endpoint} unit={unit} count={count} \
         moved={actual}"
    ));
    S_OK
}

unsafe fn text_range_move_endpoint_by_range(
    this: *mut c_void,
    endpoint: TextPatternRangeEndpoint,
    target_range: *mut c_void,
    target_endpoint: TextPatternRangeEndpoint,
) -> HRESULT {
    if target_range.is_null() {
        return E_INVALIDARG;
    }

    let provider_generation = unsafe { (*(this as *const RawTextRange)).provider_generation };
    let target = match unsafe { compatible_text_range(target_range, provider_generation) } {
        Ok(target) => unsafe { target.as_ref() },
        Err(error) => return error,
    };
    let target_offset = target.endpoint_offset(target_endpoint);
    let range = unsafe { &mut *(this as *mut RawTextRange) };

    if endpoint == TextPatternRangeEndpoint_Start {
        range.set_start(target_offset);
    } else {
        range.set_end(target_offset);
    }

    S_OK
}

unsafe fn text_range_select(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe fn text_range_add_to_selection(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe fn text_range_remove_from_selection(_this: *mut c_void) -> HRESULT {
    E_NOTIMPL
}

unsafe fn text_range_scroll_into_view(_this: *mut c_void, _align: BOOL) -> HRESULT {
    S_OK
}

unsafe fn text_range_get_children(_this: *mut c_void, children: *mut *mut SAFEARRAY) -> HRESULT {
    if children.is_null() {
        return E_POINTER;
    }

    unsafe { *children = empty_unknown_safearray() };
    trace_uia("text_range.GetChildren empty");
    S_OK
}

crate::accessibility::ffi::hresult_boundary!(
    text_provider_query_interface_ffi => text_provider_query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    )
);
crate::accessibility::ffi::u32_boundary!(
    text_provider_add_ref_ffi => text_provider_add_ref(this: *mut c_void)
);
crate::accessibility::ffi::u32_boundary!(
    text_provider_release_ffi => text_provider_release(this: *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_get_selection_ffi => text_provider_get_selection(
        this: *mut c_void,
        ranges: *mut *mut SAFEARRAY,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_get_visible_ranges_ffi => text_provider_get_visible_ranges(
        this: *mut c_void,
        ranges: *mut *mut SAFEARRAY,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_range_from_child_ffi => text_provider_range_from_child(
        this: *mut c_void,
        child: *mut c_void,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_range_from_point_ffi => text_provider_range_from_point(
        this: *mut c_void,
        point: UiaPoint,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_document_range_ffi => text_provider_document_range(
        this: *mut c_void,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_supported_text_selection_ffi => text_provider_supported_text_selection(
        this: *mut c_void,
        selection: *mut SupportedTextSelection,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_range_from_annotation_ffi => text_provider_range_from_annotation(
        this: *mut c_void,
        annotation: *mut c_void,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_provider_get_caret_range_ffi => text_provider_get_caret_range(
        this: *mut c_void,
        is_active: *mut BOOL,
        range: *mut *mut c_void,
    )
);

crate::accessibility::ffi::hresult_boundary!(
    text_range_query_interface_ffi => text_range_query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    )
);
crate::accessibility::ffi::u32_boundary!(
    text_range_add_ref_ffi => text_range_add_ref(this: *mut c_void)
);
crate::accessibility::ffi::u32_boundary!(
    text_range_release_ffi => text_range_release(this: *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_clone_ffi => text_range_clone(this: *mut c_void, range: *mut *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_compare_ffi => text_range_compare(
        this: *mut c_void,
        other: *mut c_void,
        equal: *mut BOOL,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_compare_endpoints_ffi => text_range_compare_endpoints(
        this: *mut c_void,
        endpoint: TextPatternRangeEndpoint,
        target_range: *mut c_void,
        target_endpoint: TextPatternRangeEndpoint,
        comparison: *mut i32,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_expand_to_enclosing_unit_ffi => text_range_expand_to_enclosing_unit(
        this: *mut c_void,
        unit: TextUnit,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_find_attribute_ffi => text_range_find_attribute(
        this: *mut c_void,
        attribute_id: UIA_TEXTATTRIBUTE_ID,
        value: VARIANT,
        backward: BOOL,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_find_text_ffi => text_range_find_text(
        this: *mut c_void,
        text: BSTR,
        backward: BOOL,
        ignore_case: BOOL,
        range: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_get_attribute_value_ffi => text_range_get_attribute_value(
        this: *mut c_void,
        attribute_id: UIA_TEXTATTRIBUTE_ID,
        value: *mut VARIANT,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_get_bounding_rectangles_ffi => text_range_get_bounding_rectangles(
        this: *mut c_void,
        rectangles: *mut *mut SAFEARRAY,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_get_enclosing_element_ffi => text_range_get_enclosing_element(
        this: *mut c_void,
        provider: *mut *mut c_void,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_get_text_ffi => text_range_get_text(
        this: *mut c_void,
        max_length: i32,
        text: *mut BSTR,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_move_ffi => text_range_move(
        this: *mut c_void,
        unit: TextUnit,
        count: i32,
        moved: *mut i32,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_move_endpoint_by_unit_ffi => text_range_move_endpoint_by_unit(
        this: *mut c_void,
        endpoint: TextPatternRangeEndpoint,
        unit: TextUnit,
        count: i32,
        moved: *mut i32,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_move_endpoint_by_range_ffi => text_range_move_endpoint_by_range(
        this: *mut c_void,
        endpoint: TextPatternRangeEndpoint,
        target_range: *mut c_void,
        target_endpoint: TextPatternRangeEndpoint,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_select_ffi => text_range_select(this: *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_add_to_selection_ffi => text_range_add_to_selection(this: *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_remove_from_selection_ffi => text_range_remove_from_selection(this: *mut c_void)
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_scroll_into_view_ffi => text_range_scroll_into_view(
        this: *mut c_void,
        align: BOOL,
    )
);
crate::accessibility::ffi::hresult_boundary!(
    text_range_get_children_ffi => text_range_get_children(
        this: *mut c_void,
        children: *mut *mut SAFEARRAY,
    )
);

unsafe fn single_unknown_safearray(value: *mut c_void) -> *mut SAFEARRAY {
    let array = unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, 1) };
    if array.is_null() {
        return ptr::null_mut();
    }

    let _ = unsafe { SafeArraySetIID(array, &IID_IUNKNOWN) };
    let index = 0;
    // SAFETY: The SAFEARRAY stores VT_UNKNOWN interface pointers. `SafeArrayPutElement` AddRefs
    // the supplied COM object on success.
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

fn move_offset_by_unit(text: &str, offset: usize, unit: TextUnit, count: i32) -> (usize, i32) {
    if unit == TextUnit_Character {
        move_offset_by_boundaries(text, offset, count, char_boundaries(text))
    } else if unit == TextUnit_Word {
        move_offset_by_boundaries(text, offset, count, word_starts(text))
    } else if is_terminal_line_unit(unit) {
        move_offset_by_boundaries(text, offset, count, line_starts(text))
    } else if unit == TextUnit_Document {
        if count > 0 && offset < text.len() {
            (text.len(), 1)
        } else if count < 0 && offset > 0 {
            (0, -1)
        } else {
            (offset, 0)
        }
    } else {
        (offset, 0)
    }
}

fn normalized_unit_start(text: &str, offset: usize, unit: TextUnit) -> usize {
    if unit == TextUnit_Character {
        previous_character_boundary(text, offset)
    } else if unit == TextUnit_Word {
        word_bounds(text, offset).0
    } else if is_terminal_line_unit(unit) {
        line_bounds(text, offset).0
    } else {
        offset
    }
}

fn move_offset_by_boundaries(
    _text: &str,
    offset: usize,
    count: i32,
    mut boundaries: Vec<usize>,
) -> (usize, i32) {
    boundaries.sort_unstable();
    boundaries.dedup();
    let current = boundaries.partition_point(|boundary| *boundary < offset);

    if count >= 0 {
        let target = (current + count as usize).min(boundaries.len().saturating_sub(1));
        (boundaries[target], target.saturating_sub(current) as i32)
    } else {
        let target = current.saturating_sub((-count) as usize);
        (boundaries[target], -(current.saturating_sub(target) as i32))
    }
}

fn char_boundaries(text: &str) -> Vec<usize> {
    text.grapheme_indices(true).map(|(index, _)| index).chain([text.len()]).collect()
}

fn previous_character_boundary(text: &str, offset: usize) -> usize {
    char_boundaries(text).into_iter().take_while(|index| *index <= offset).last().unwrap_or(0)
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    starts.push(text.len());
    starts
}

fn is_terminal_line_unit(unit: TextUnit) -> bool {
    unit == TextUnit_Line || unit == TextUnit_Paragraph
}

fn word_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut in_word = false;
    for (index, character) in text.char_indices() {
        if is_word_character(character) {
            if !in_word {
                starts.push(index);
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    starts.push(text.len());
    starts
}

fn line_bounds(text: &str, offset: usize) -> (usize, usize) {
    let offset = previous_char_boundary(text, offset.min(text.len()));
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |index| offset + index);
    (start, end)
}

fn offset_for_line_column(text: &str, row: usize, column: usize) -> usize {
    let line_start = line_starts(text).get(row).copied().unwrap_or(text.len());
    let line_end = text[line_start..].find('\n').map_or(text.len(), |index| line_start + index);

    text[line_start..line_end]
        .char_indices()
        .nth(column)
        .map_or(line_end, |(index, _)| line_start + index)
}

fn caret_word_bounds(text: &str, offset: usize) -> (usize, usize) {
    let offset = previous_char_boundary(text, offset.min(text.len()));
    if offset < text.len()
        && !text[offset..].chars().next().is_some_and(is_word_character)
        && offset > 0
    {
        let previous = previous_char_boundary(text, offset - 1);
        if text[previous..offset].chars().next().is_some_and(is_word_character)
            && let Some(next_word) =
                text[offset..].char_indices().find(|(_, character)| is_word_character(*character))
        {
            return word_bounds(text, offset + next_word.0);
        }
    }

    word_bounds(text, offset)
}

fn word_bounds(text: &str, offset: usize) -> (usize, usize) {
    let offset = previous_char_boundary(text, offset.min(text.len()));
    let mut start = offset;
    while start > 0 {
        let previous = previous_char_boundary(text, start - 1);
        let character = text[previous..start].chars().next().unwrap();
        if !is_word_character(character) {
            break;
        }
        start = previous;
    }

    if start == offset
        && text[start..].chars().next().is_none_or(|character| !is_word_character(character))
    {
        start = text[offset..]
            .char_indices()
            .find(|(_, character)| is_word_character(*character))
            .map_or(text.len(), |(index, _)| offset + index);
    }

    let mut end = start;
    for (index, character) in text[start..].char_indices() {
        if !is_word_character(character) {
            break;
        }
        end = start + index + character.len_utf8();
    }

    (start, end)
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn previous_char_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index > offset)
        .unwrap_or(text.len())
}

fn clamp_to_boundary(text: &str, offset: usize) -> usize {
    previous_char_boundary(text, offset.min(text.len()))
}

fn normalized_range_bounds(text: &str, start: usize, end: usize) -> (usize, usize) {
    let start = clamp_to_boundary(text, start);
    let mut end = clamp_to_boundary(text, end);
    if end < start {
        end = start;
    }

    (start, end)
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

unsafe fn compatible_text_range(
    range: *mut c_void,
    provider_generation: u64,
) -> Result<NonNull<RawTextRange>, HRESULT> {
    let range = NonNull::new(range.cast::<RawTextRange>()).ok_or(E_INVALIDARG)?;

    // SAFETY: COM requires a non-null interface argument to remain valid for this callback. Only
    // the interface's first vtable word is read until local implementation identity is proven.
    let vtable = unsafe { *(range.as_ptr() as *const *const RawTextRangeVtable) };
    if !ptr::eq(vtable, ptr::addr_of!(RAW_TEXT_RANGE_VTABLE)) {
        return Err(E_INVALIDARG);
    }

    if unsafe { range.as_ref().provider_generation } != provider_generation {
        return Err(E_INVALIDARG);
    }

    Ok(range)
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use windows_sys::Win32::Foundation::{S_OK, SysFreeString, SysStringLen};
    use windows_sys::Win32::System::Ole::{
        SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetVartype,
    };
    use windows_sys::Win32::System::Variant::VT_R8;
    use windows_sys::Win32::UI::Accessibility::{
        SupportedTextSelection_Multiple, TextPatternRangeEndpoint_Start,
    };
    use windows_sys::core::BSTR;

    use super::{IID_ITEXT_PROVIDER2, RawTextProvider};

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
    fn text_range_clamps_non_boundary_offsets_before_slicing() {
        let text = "a\u{e9}z".to_owned();
        let range = super::RawTextRange::allocate(text, 0, 2, ptr::null_mut(), 0);

        let selected = std::panic::catch_unwind(|| unsafe { (*range.as_ptr()).selected_text(-1) });
        unsafe {
            super::text_range_release(range.as_ptr().cast());
        }

        assert_eq!(selected.expect("text range selection should not panic"), "a");
    }

    #[test]
    fn foreign_text_range_is_rejected() {
        let source = super::RawTextRange::allocate("same".to_owned(), 0, 1, ptr::null_mut(), 0);
        let foreign = super::RawTextRange::allocate("same".to_owned(), 1, 2, ptr::null_mut(), 0);
        let foreign_vtable = Box::leak(Box::new(unsafe {
            // SAFETY: The vtable contains only function pointers and has no drop state. The copy
            // provides a valid but distinct COM implementation identity for this regression test.
            ptr::read(&super::RAW_TEXT_RANGE_VTABLE)
        }));
        unsafe {
            (*foreign.as_ptr()).vtable = foreign_vtable;

            let source_vtable = (*source.as_ptr()).vtable;
            let mut equal = 1;
            assert_eq!(
                (source_vtable.compare)(
                    source.as_ptr().cast(),
                    foreign.as_ptr().cast(),
                    &mut equal
                ),
                super::E_INVALIDARG,
            );
            assert_eq!(equal, 0);

            let mut comparison = 123;
            assert_eq!(
                (source_vtable.compare_endpoints)(
                    source.as_ptr().cast(),
                    TextPatternRangeEndpoint_Start,
                    foreign.as_ptr().cast(),
                    TextPatternRangeEndpoint_Start,
                    &mut comparison,
                ),
                super::E_INVALIDARG,
            );

            assert_eq!(
                (source_vtable.move_endpoint_by_range)(
                    source.as_ptr().cast(),
                    TextPatternRangeEndpoint_Start,
                    foreign.as_ptr().cast(),
                    TextPatternRangeEndpoint_Start,
                ),
                super::E_INVALIDARG,
            );
            assert_eq!((*source.as_ptr()).start, 0);

            super::text_range_release(foreign.as_ptr().cast());
            super::text_range_release(source.as_ptr().cast());
        }
    }

    #[test]
    fn stale_text_range_is_rejected() {
        let first_provider = RawTextProvider::allocate("same".to_owned());
        let second_provider = RawTextProvider::allocate("same".to_owned());

        unsafe {
            let first_vtable = (*first_provider.as_ptr()).vtable;
            let second_vtable = (*second_provider.as_ptr()).vtable;
            let mut first_range = ptr::null_mut();
            let mut second_range = ptr::null_mut();
            assert_eq!(
                (first_vtable.document_range)(first_provider.as_ptr().cast(), &mut first_range),
                S_OK,
            );
            assert_eq!(
                (second_vtable.document_range)(second_provider.as_ptr().cast(), &mut second_range),
                S_OK,
            );

            let range_vtable = *(second_range as *mut &'static super::RawTextRangeVtable);
            let mut equal = 1;
            assert_eq!(
                (range_vtable.compare)(second_range, first_range, &mut equal),
                super::E_INVALIDARG,
            );
            assert_eq!(equal, 0);

            let mut comparison = 123;
            assert_eq!(
                (range_vtable.compare_endpoints)(
                    second_range,
                    TextPatternRangeEndpoint_Start,
                    first_range,
                    TextPatternRangeEndpoint_Start,
                    &mut comparison,
                ),
                super::E_INVALIDARG,
            );

            assert_eq!(
                (range_vtable.move_endpoint_by_range)(
                    second_range,
                    TextPatternRangeEndpoint_Start,
                    first_range,
                    TextPatternRangeEndpoint_Start,
                ),
                super::E_INVALIDARG,
            );
            assert_eq!((*(second_range as *const super::RawTextRange)).start, 0);

            super::text_range_release(first_range);
            super::text_range_release(second_range);
            (first_vtable.release)(first_provider.as_ptr().cast());
            (second_vtable.release)(second_provider.as_ptr().cast());
        }
    }

    #[test]
    fn poisoned_provider_state_is_contained() {
        let provider = RawTextProvider::allocate("text".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let _state = (*provider.as_ptr()).state.write().unwrap();
            panic!("poison provider state");
        }));
        assert!(poisoned.is_err());

        unsafe {
            let mut range = ptr::null_mut();
            assert_eq!(
                (vtable.document_range)(raw_provider, &mut range),
                windows_sys::Win32::Foundation::E_FAIL,
            );
            assert!(range.is_null());
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_pattern_capabilities_match_implementation() {
        let provider = RawTextProvider::allocate("visible viewport".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            (*provider.as_ptr()).set_focused(true);

            let mut selection = 1;
            assert_eq!((vtable.supported_text_selection)(raw_provider, &mut selection), 0);
            assert_eq!(selection, SupportedTextSelection_Multiple);

            let mut document_range = ptr::null_mut();
            assert_eq!((vtable.document_range)(raw_provider, &mut document_range), S_OK);
            let range_vtable = *(document_range as *mut &'static super::RawTextRangeVtable);
            let mut text: BSTR = ptr::null_mut();
            assert_eq!((range_vtable.get_text)(document_range, -1, &mut text), S_OK);
            assert_eq!(
                String::from_utf16_lossy(std::slice::from_raw_parts(
                    text,
                    SysStringLen(text) as usize,
                )),
                "visible viewport"
            );
            SysFreeString(text);
            let mut is_active = 0;
            let mut caret_range = ptr::null_mut();
            assert_eq!(
                (vtable.get_caret_range)(raw_provider, &mut is_active, &mut caret_range),
                S_OK
            );
            assert_eq!(is_active, 1);
            assert!(!caret_range.is_null());

            (range_vtable.release)(caret_range);
            (range_vtable.release)(document_range);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_provider_supports_text_pattern2_query_interface() {
        let provider = RawTextProvider::allocate("text".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut text_provider2 = std::ptr::null_mut();
            assert_eq!(
                (vtable.query_interface)(raw_provider, &IID_ITEXT_PROVIDER2, &mut text_provider2),
                0,
            );
            assert_eq!(text_provider2, raw_provider);

            (vtable.release)(text_provider2);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_provider2_get_caret_range_returns_collapsed_active_range() {
        let provider = RawTextProvider::allocate("alpha beta".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            (*provider.as_ptr()).set_focused(true);
            let mut is_active = 0;
            let mut range = std::ptr::null_mut();
            assert_eq!((vtable.get_caret_range)(raw_provider, &mut is_active, &mut range), 0);
            assert_eq!(is_active, 1);
            assert!(!range.is_null());

            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            let mut text: BSTR = std::ptr::null_mut();
            assert_eq!((range_vtable.get_text)(range, -1, &mut text), 0);
            assert_eq!(SysStringLen(text), 0);

            SysFreeString(text);
            (range_vtable.release)(range);
            (vtable.release)(raw_provider);
        }
    }

    #[test]
    fn text_provider2_get_caret_range_reflects_focus() {
        let provider = RawTextProvider::allocate("alpha beta".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut is_active = 1;
            let mut range = std::ptr::null_mut();
            assert_eq!((vtable.get_caret_range)(raw_provider, &mut is_active, &mut range), 0);
            assert_eq!(is_active, 0);
            assert!(!range.is_null());

            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            (range_vtable.release)(range);

            (*provider.as_ptr()).set_focused(true);
            assert_eq!((vtable.get_caret_range)(raw_provider, &mut is_active, &mut range), 0);
            assert_eq!(is_active, 1);
            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            (range_vtable.release)(range);

            (*provider.as_ptr()).set_focused(false);
            assert_eq!((vtable.get_caret_range)(raw_provider, &mut is_active, &mut range), 0);
            assert_eq!(is_active, 0);
            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            (range_vtable.release)(range);
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

    #[test]
    fn bounding_rectangles_returns_double_array_even_when_empty() {
        let provider = RawTextProvider::allocate("visible viewport".to_owned());
        let raw_provider = provider.as_ptr().cast();
        let vtable = unsafe { (*provider.as_ptr()).vtable };

        unsafe {
            let mut range = std::ptr::null_mut();
            assert_eq!((vtable.document_range)(raw_provider, &mut range), 0);
            assert!(!range.is_null());

            let range_vtable = *(range as *mut &'static super::RawTextRangeVtable);
            let mut rectangles = std::ptr::null_mut();
            assert_eq!((range_vtable.get_bounding_rectangles)(range, &mut rectangles), 0);
            assert!(!rectangles.is_null());

            let mut vartype = 0;
            assert_eq!(SafeArrayGetVartype(rectangles, &mut vartype), 0);
            assert_eq!(vartype, VT_R8);

            SafeArrayDestroy(rectangles);
            (range_vtable.release)(range);
            (vtable.release)(raw_provider);
        }
    }
}
