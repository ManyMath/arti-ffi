use log::{error, warn};
use std::cell::RefCell;
use std::error::Error;
use std::ffi::{c_char, CString};

#[macro_export]
macro_rules! unwrap_or_return {
    // Macro to handle errors and return a fallback value.
    ($result:expr, $fallback:expr) => {
        match $result {
            Ok(value) => value,
            Err(e) => {
                crate::error::update_last_error(e); // Update last error before returning.
                return $fallback;
            }
        }
    };
}

thread_local! {
    // Thread-local storage for the last error encountered.
    static LAST_ERROR: RefCell<Option<Box<dyn Error>>> = RefCell::new(None);
}

#[no_mangle]
pub unsafe extern "C" fn arti_last_error_message() -> *const c_char {
    // Retrieve the last error message and convert it to a C string.
    let last_error = match crate::error::take_last_error() {
        Some(err) => err,
        None => return CString::new("").unwrap().into_raw(),
    };

    let error_message = last_error.to_string();
    CString::new(error_message).unwrap().into_raw()
}

pub fn update_last_error<E: Error + 'static>(err: E) {
    error!("Setting LAST_ERROR: {}", err);

    // Log the chain of causes for the error.
    let mut cause = err.source();
    while let Some(parent_err) = cause {
        warn!("Caused by: {}", parent_err);
        cause = parent_err.source();
    }

    LAST_ERROR.with(|prev| {
        *prev.borrow_mut() = Some(Box::new(err)); // Store the last error in thread-local storage.
    });
}

pub fn take_last_error() -> Option<Box<dyn Error>> {
    // Retrieve and clear the last error from thread-local storage.
    LAST_ERROR.with(|prev| prev.borrow_mut().take())
}
