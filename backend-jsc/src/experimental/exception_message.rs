// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{JsError, JsException, JsString};
use crate::sys;

const MAX_MESSAGE_BYTES: usize = 4096;
const TRUNCATION_SUFFIX: &str = "… [truncated]";

pub(super) fn copy(string: &JsString) -> Result<JsException, JsError> {
    // SAFETY: JsString owns this immutable string reference for all calls below.
    let maximum = unsafe { sys::string_maximum_utf8_size(string.as_ptr()) };
    if maximum == 0 {
        return Err(JsError::Backend(
            "JavaScriptCore reported an invalid string size",
        ));
    }
    let capacity = maximum.min(MAX_MESSAGE_BYTES + 1);
    let mut bytes = vec![0_u8; capacity];
    // SAFETY: JSC permits partial conversion into a smaller buffer. All capacity
    // bytes are writable, including space for the terminating NUL.
    let written =
        unsafe { sys::string_get_utf8(string.as_ptr(), bytes.as_mut_ptr().cast(), capacity) };
    if written == 0 || written > capacity {
        return Err(JsError::Backend("JavaScriptCore string conversion failed"));
    }
    bytes.truncate(written - 1);
    let mut message =
        String::from_utf8(bytes).map_err(|_| JsError::Backend("JSC produced invalid UTF-8"))?;
    // SAFETY: The owned JSString remains live. JSC reports UTF-16 code units,
    // not the worst-case UTF-8 capacity returned by maximum_utf8_size.
    let original_units = unsafe { sys::string_length(string.as_ptr()) };
    let copied_units = if message.is_ascii() {
        message.len()
    } else {
        message.encode_utf16().count()
    };
    let truncated = copied_units < original_units;
    if truncated {
        let mut end = message
            .len()
            .min(MAX_MESSAGE_BYTES - TRUNCATION_SUFFIX.len());
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str(TRUNCATION_SUFFIX);
    }
    Ok(JsException { message, truncated })
}
