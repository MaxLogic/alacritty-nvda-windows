use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};

use log::error;
use windows_sys::Win32::Foundation::{E_FAIL, LRESULT};
use windows_sys::core::HRESULT;

const MAX_PANIC_LOGS: u32 = 8;
static PANIC_LOGS: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static BOUNDARY_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct BoundaryGuard;

impl BoundaryGuard {
    fn enter() -> Self {
        BOUNDARY_DEPTH.set(BOUNDARY_DEPTH.get().saturating_add(1));
        Self
    }
}

impl Drop for BoundaryGuard {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.set(BOUNDARY_DEPTH.get().saturating_sub(1));
    }
}

pub(crate) fn is_boundary_active() -> bool {
    BOUNDARY_DEPTH.get() > 0
}

fn report_panic(name: &'static str) {
    let should_log = PANIC_LOGS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < MAX_PANIC_LOGS).then_some(count + 1)
        })
        .is_ok();

    if should_log {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            error!("panic contained at accessibility FFI boundary {name}");
        }));
    }
}

fn catch_or<T>(name: &'static str, fallback: T, callback: impl FnOnce() -> T) -> T {
    let _guard = BoundaryGuard::enter();
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            report_panic(name);
            fallback
        },
    }
}

pub(crate) fn catch_hresult(name: &'static str, callback: impl FnOnce() -> HRESULT) -> HRESULT {
    catch_or(name, E_FAIL, callback)
}

pub(crate) fn catch_u32(name: &'static str, callback: impl FnOnce() -> u32) -> u32 {
    catch_or(name, 0, callback)
}

pub(crate) fn catch_lresult(
    name: &'static str,
    fallback: impl FnOnce() -> LRESULT,
    callback: impl FnOnce() -> LRESULT,
) -> LRESULT {
    let _guard = BoundaryGuard::enter();
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            report_panic(name);
            catch_unwind(AssertUnwindSafe(fallback)).unwrap_or_default()
        },
    }
}

macro_rules! hresult_boundary {
    ($wrapper:ident => $implementation:ident($($argument:ident: $argument_type:ty),* $(,)?)) => {
        unsafe extern "system" fn $wrapper(
            $($argument: $argument_type),*
        ) -> windows_sys::core::HRESULT {
            $crate::accessibility::ffi::catch_hresult(stringify!($implementation), || unsafe {
                // SAFETY: The wrapper forwards the ABI callback arguments unchanged and the
                // implementation retains the callback's original unsafe preconditions.
                $implementation($($argument),*)
            })
        }
    };
}

macro_rules! u32_boundary {
    ($wrapper:ident => $implementation:ident($($argument:ident: $argument_type:ty),* $(,)?)) => {
        unsafe extern "system" fn $wrapper($($argument: $argument_type),*) -> u32 {
            $crate::accessibility::ffi::catch_u32(stringify!($implementation), || unsafe {
                // SAFETY: The wrapper forwards the ABI callback arguments unchanged and the
                // implementation retains the callback's original unsafe preconditions.
                $implementation($($argument),*)
            })
        }
    };
}

pub(crate) use hresult_boundary;
pub(crate) use u32_boundary;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    #[test]
    fn ffi_panic_returns_failure() {
        assert!(!super::is_boundary_active());
        assert_eq!(
            super::catch_hresult("test.callback", || {
                assert!(super::is_boundary_active());
                panic!("test panic");
            }),
            windows_sys::Win32::Foundation::E_FAIL,
        );
        assert!(!super::is_boundary_active());
    }

    #[test]
    fn ffi_panic_returns_zero_for_refcount_callback() {
        assert_eq!(super::catch_u32("test.refcount", || panic!("test panic")), 0);
    }

    #[test]
    fn ffi_panic_invokes_subclass_fallback() {
        let fallback_called = Cell::new(false);

        let result = super::catch_lresult(
            "test.subclass",
            || {
                fallback_called.set(true);
                42
            },
            || panic!("test panic"),
        );

        assert_eq!(result, 42);
        assert!(fallback_called.get());
    }

    #[test]
    fn nested_boundary_depth_is_restored() {
        assert!(!super::is_boundary_active());
        assert_eq!(
            super::catch_hresult("test.outer", || {
                assert!(super::is_boundary_active());
                assert_eq!(
                    super::catch_hresult("test.inner", || panic!("test panic")),
                    windows_sys::Win32::Foundation::E_FAIL,
                );
                assert!(super::is_boundary_active());
                windows_sys::Win32::Foundation::S_OK
            }),
            windows_sys::Win32::Foundation::S_OK,
        );
        assert!(!super::is_boundary_active());
    }
}
