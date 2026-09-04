// SPDX-License-Identifier: MIT OR Apache-2.0

use super::Runtime;

#[test]
fn full_local_budget_rejects_before_javascript_side_effects() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            for _ in 0..4096 {
                let _ = cx.eval("({})", "fill.js").unwrap();
            }
            assert!(
                cx.eval("globalThis.quotaBypassed = true; ({})", "reject.js")
                    .is_err()
            );
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            let value = cx
                .eval("globalThis.quotaBypassed === undefined", "check.js")
                .unwrap();
            assert!(cx.boolean(&value).unwrap());
        })
        .unwrap();
}
