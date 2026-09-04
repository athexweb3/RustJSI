// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::sys;
use std::ptr::NonNull;

const INLINE_LOCAL_ROOTS: usize = 16;

pub(super) struct LocalRoots {
    inline: [Option<NonNull<sys::OpaqueValue>>; INLINE_LOCAL_ROOTS],
    inline_len: usize,
    spill: Vec<NonNull<sys::OpaqueValue>>,
}

impl LocalRoots {
    pub(super) const fn new() -> Self {
        Self {
            inline: [None; INLINE_LOCAL_ROOTS],
            inline_len: 0,
            spill: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, value: NonNull<sys::OpaqueValue>) {
        if self.inline_len < INLINE_LOCAL_ROOTS {
            self.inline[self.inline_len] = Some(value);
            self.inline_len += 1;
        } else {
            self.spill.push(value);
        }
    }

    pub(super) fn drain(&mut self) -> impl Iterator<Item = NonNull<sys::OpaqueValue>> + '_ {
        let inline = self.inline[..self.inline_len]
            .iter_mut()
            .filter_map(Option::take);
        self.inline_len = 0;
        inline.chain(self.spill.drain(..))
    }
}
