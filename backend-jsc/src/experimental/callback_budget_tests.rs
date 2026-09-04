// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Runtime, Value};

#[test]
fn full_callback_registry_rejects_before_publication() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            cx.eval(
                "globalThis.published = 0; Object.defineProperty(globalThis, 'overflow', { set(_) { ++published; } });",
                "setter.js",
            )
            .unwrap();
            for _ in 0..4096 {
                cx.install_host_function("retained", |_| Ok(Value::Undefined))
                    .unwrap();
            }
            assert!(
                cx.install_host_function("overflow", |_| Ok(Value::Undefined))
                    .is_err()
            );
            let untouched = cx.eval("published === 0", "check.js").unwrap();
            assert!(cx.boolean(&untouched).unwrap());
        })
        .unwrap();
}
