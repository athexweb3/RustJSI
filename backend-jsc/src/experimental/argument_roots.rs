// SPDX-License-Identifier: MIT OR Apache-2.0

use super::Context;
use crate::sys;
use std::ptr::NonNull;

pub(super) struct ArgumentRoot<'scope, 'cx> {
    context: &'scope Context<'cx>,
    value: NonNull<sys::OpaqueValue>,
}

impl<'scope, 'cx> ArgumentRoot<'scope, 'cx> {
    pub(super) fn new(context: &'scope Context<'cx>, value: NonNull<sys::OpaqueValue>) -> Self {
        // SAFETY: The value was created in this active Context and remains
        // protected until the synchronous call and result capture finish.
        unsafe { sys::value_protect(context.raw.as_ptr(), value.as_ptr()) };
        #[cfg(test)]
        context
            .shared
            .argument_roots
            .set(context.shared.argument_roots.get() + 1);
        Self { context, value }
    }
}

impl Drop for ArgumentRoot<'_, '_> {
    fn drop(&mut self) {
        // SAFETY: This root is dropped inside Context::call while its host entry
        // and engine context are still live. Each constructor protects once.
        unsafe { sys::value_unprotect(self.context.raw.as_ptr(), self.value.as_ptr()) };
        #[cfg(test)]
        self.context
            .shared
            .argument_roots
            .set(self.context.shared.argument_roots.get() - 1);
    }
}
