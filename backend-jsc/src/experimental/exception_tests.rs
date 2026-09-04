// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{JsError, Runtime};

#[test]
fn large_exception_has_a_bounded_rust_message() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            let error = cx.eval("throw 'x'.repeat(512 * 1024)", "large.js").unwrap_err();
            let JsError::Exception(error) = error else {
                panic!("expected JavaScript exception");
            };
            assert!(error.message().len() <= 4096);
        })
        .unwrap();
}
