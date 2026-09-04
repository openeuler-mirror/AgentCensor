use std::ffi::{c_char, c_int, c_void};

pub type KernelHandle = c_void;
pub type EventReaderHandle = c_void;

unsafe extern "C" {
    pub fn as_kernel_load(
        path: *const c_char,
        programs: *const *const c_char,
        count: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> *mut KernelHandle;

    pub fn as_kernel_free(kernel: *mut KernelHandle);

    pub fn as_kernel_map_fd(kernel: *const KernelHandle, name: *const c_char) -> c_int;

    pub fn as_kernel_update_map(
        kernel: *const KernelHandle,
        name: *const c_char,
        key: *const c_void,
        key_size: usize,
        value: *const c_void,
        value_size: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_kernel_lookup_map(
        kernel: *const KernelHandle,
        name: *const c_char,
        key: *const c_void,
        key_size: usize,
        value: *mut c_void,
        value_size: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_kernel_replace_inner(
        kernel: *const KernelHandle,
        outer_name: *const c_char,
        slot: u32,
        keys: *const c_void,
        values: *const c_void,
        count: usize,
        key_size: usize,
        value_size: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_kernel_clone_rule_slot(
        kernel: *const KernelHandle,
        source_slot: u32,
        destination_slot: u32,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_kernel_delete_map(
        kernel: *const KernelHandle,
        name: *const c_char,
        key: *const c_void,
        key_size: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_kernel_map_count(
        kernel: *const KernelHandle,
        name: *const c_char,
        error: *mut c_char,
        error_size: usize,
    ) -> isize;

    pub fn as_kernel_list_scope(
        kernel: *const KernelHandle,
        name: *const c_char,
        scope_id: u64,
        pids: *mut u32,
        capacity: usize,
        error: *mut c_char,
        error_size: usize,
    ) -> isize;

    pub fn as_event_reader_new(
        kernel: *const KernelHandle,
        error: *mut c_char,
        error_size: usize,
    ) -> *mut EventReaderHandle;

    pub fn as_event_reader_free(reader: *mut EventReaderHandle);

    pub fn as_event_reader_next(
        reader: *mut EventReaderHandle,
        output: *mut c_void,
        output_size: usize,
        timeout_ms: c_int,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;

    pub fn as_event_reader_dropped(reader: *const EventReaderHandle) -> usize;
    pub fn as_event_reader_kernel_dropped(reader: *const EventReaderHandle) -> usize;
}
