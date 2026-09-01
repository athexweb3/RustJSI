// SPDX-License-Identifier: MIT OR Apache-2.0

use std::ffi::{c_int, c_uint, c_void};

pub(crate) enum OpaqueClass {}
pub(crate) enum OpaqueContext {}
pub(crate) enum OpaqueString {}
pub(crate) enum OpaqueValue {}

pub(crate) type ClassRef = *mut OpaqueClass;
pub(crate) type ContextRef = *const OpaqueContext;
pub(crate) type GlobalContextRef = *mut OpaqueContext;
pub(crate) type StringRef = *mut OpaqueString;
pub(crate) type ValueRef = *const OpaqueValue;
pub(crate) type ObjectRef = *mut OpaqueValue;

pub(crate) type FunctionCallback = Option<
    unsafe extern "C" fn(
        ContextRef,
        ObjectRef,
        ObjectRef,
        usize,
        *const ValueRef,
        *mut ValueRef,
    ) -> ValueRef,
>;

pub(crate) type FinalizeCallback = Option<unsafe extern "C" fn(ObjectRef)>;
pub(crate) type TypedArrayBytesDeallocator = Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>;

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct ClassDefinition {
    pub(crate) version: c_int,
    pub(crate) attributes: c_uint,
    pub(crate) class_name: *const c_void,
    pub(crate) parent_class: ClassRef,
    pub(crate) static_values: *const c_void,
    pub(crate) static_functions: *const c_void,
    pub(crate) initialize: *const c_void,
    pub(crate) finalize: FinalizeCallback,
    pub(crate) has_property: *const c_void,
    pub(crate) get_property: *const c_void,
    pub(crate) set_property: *const c_void,
    pub(crate) delete_property: *const c_void,
    pub(crate) get_property_names: *const c_void,
    pub(crate) call_as_function: *const c_void,
    pub(crate) call_as_constructor: *const c_void,
    pub(crate) has_instance: *const c_void,
    pub(crate) convert_to_type: *const c_void,
}

#[link(name = "JavaScriptCore", kind = "framework")]
unsafe extern "C" {
    #[link_name = "kJSClassDefinitionEmpty"]
    pub(crate) static CLASS_DEFINITION_EMPTY: ClassDefinition;

    #[link_name = "JSClassCreate"]
    pub(crate) fn class_create(definition: *const ClassDefinition) -> ClassRef;

    #[link_name = "JSClassRelease"]
    pub(crate) fn class_release(class: ClassRef);

    #[link_name = "JSGlobalContextCreate"]
    pub(crate) fn global_context_create(class: *mut ()) -> GlobalContextRef;

    #[link_name = "JSGlobalContextRelease"]
    pub(crate) fn global_context_release(context: GlobalContextRef);

    #[link_name = "JSContextGetGlobalObject"]
    pub(crate) fn context_get_global_object(context: ContextRef) -> ObjectRef;

    #[link_name = "JSEvaluateScript"]
    pub(crate) fn evaluate_script(
        context: ContextRef,
        script: StringRef,
        this_object: ObjectRef,
        source_url: StringRef,
        starting_line_number: c_int,
        exception: *mut ValueRef,
    ) -> ValueRef;

    #[link_name = "JSGarbageCollect"]
    pub(crate) fn garbage_collect(context: ContextRef);

    #[link_name = "JSStringCreateWithCharacters"]
    pub(crate) fn string_create_with_characters(chars: *const u16, length: usize) -> StringRef;

    #[link_name = "JSStringRelease"]
    pub(crate) fn string_release(string: StringRef);

    #[link_name = "JSStringGetMaximumUTF8CStringSize"]
    pub(crate) fn string_maximum_utf8_size(string: StringRef) -> usize;

    #[link_name = "JSStringGetUTF8CString"]
    pub(crate) fn string_get_utf8(string: StringRef, buffer: *mut i8, buffer_size: usize) -> usize;

    #[link_name = "JSValueMakeUndefined"]
    pub(crate) fn value_make_undefined(context: ContextRef) -> ValueRef;

    #[link_name = "JSValueMakeNull"]
    pub(crate) fn value_make_null(context: ContextRef) -> ValueRef;

    #[link_name = "JSValueMakeBoolean"]
    pub(crate) fn value_make_boolean(context: ContextRef, value: bool) -> ValueRef;

    #[link_name = "JSValueMakeNumber"]
    pub(crate) fn value_make_number(context: ContextRef, value: f64) -> ValueRef;

    #[link_name = "JSValueMakeString"]
    pub(crate) fn value_make_string(context: ContextRef, value: StringRef) -> ValueRef;

    #[link_name = "JSValueIsBoolean"]
    pub(crate) fn value_is_boolean(context: ContextRef, value: ValueRef) -> bool;

    #[link_name = "JSValueIsNumber"]
    pub(crate) fn value_is_number(context: ContextRef, value: ValueRef) -> bool;

    #[link_name = "JSValueToBoolean"]
    pub(crate) fn value_to_boolean(context: ContextRef, value: ValueRef) -> bool;

    #[link_name = "JSValueToNumber"]
    pub(crate) fn value_to_number(
        context: ContextRef,
        value: ValueRef,
        exception: *mut ValueRef,
    ) -> f64;

    #[link_name = "JSValueToStringCopy"]
    pub(crate) fn value_to_string_copy(
        context: ContextRef,
        value: ValueRef,
        exception: *mut ValueRef,
    ) -> StringRef;

    #[link_name = "JSValueProtect"]
    pub(crate) fn value_protect(context: ContextRef, value: ValueRef);

    #[link_name = "JSValueUnprotect"]
    pub(crate) fn value_unprotect(context: ContextRef, value: ValueRef);

    #[link_name = "JSObjectMakeFunctionWithCallback"]
    pub(crate) fn object_make_function_with_callback(
        context: ContextRef,
        name: StringRef,
        callback: FunctionCallback,
    ) -> ObjectRef;

    #[link_name = "JSObjectMake"]
    pub(crate) fn object_make(
        context: ContextRef,
        class: ClassRef,
        private_data: *mut c_void,
    ) -> ObjectRef;

    #[link_name = "JSObjectGetPrivate"]
    pub(crate) fn object_get_private(object: ObjectRef) -> *mut c_void;

    #[link_name = "JSObjectSetPrivate"]
    pub(crate) fn object_set_private(object: ObjectRef, private_data: *mut c_void) -> bool;

    #[link_name = "JSObjectMakeArrayBufferWithBytesNoCopy"]
    pub(crate) fn object_make_array_buffer_with_bytes_no_copy(
        context: ContextRef,
        bytes: *mut c_void,
        byte_length: usize,
        deallocator: TypedArrayBytesDeallocator,
        deallocator_context: *mut c_void,
        exception: *mut ValueRef,
    ) -> ObjectRef;

    #[link_name = "JSObjectGetArrayBufferBytesPtr"]
    pub(crate) fn object_get_array_buffer_bytes_ptr(
        context: ContextRef,
        object: ObjectRef,
        exception: *mut ValueRef,
    ) -> *mut c_void;

    #[link_name = "JSObjectGetArrayBufferByteLength"]
    pub(crate) fn object_get_array_buffer_byte_length(
        context: ContextRef,
        object: ObjectRef,
        exception: *mut ValueRef,
    ) -> usize;

    #[link_name = "JSObjectSetProperty"]
    pub(crate) fn object_set_property(
        context: ContextRef,
        object: ObjectRef,
        property_name: StringRef,
        value: ValueRef,
        attributes: c_uint,
        exception: *mut ValueRef,
    );

    #[link_name = "JSObjectCallAsFunction"]
    pub(crate) fn object_call_as_function(
        context: ContextRef,
        object: ObjectRef,
        this_object: ObjectRef,
        argument_count: usize,
        arguments: *const ValueRef,
        exception: *mut ValueRef,
    ) -> ValueRef;

    #[link_name = "JSObjectMakeError"]
    pub(crate) fn object_make_error(
        context: ContextRef,
        argument_count: usize,
        arguments: *const ValueRef,
        exception: *mut ValueRef,
    ) -> ObjectRef;
}
