// wasm_entry.rs — WASM entry point for unit
//
// Provides a C-compatible API for the browser to create and interact
// with a unit VM instance. The VM is heap-allocated and accessed via
// raw pointer. Strings are passed through WASM linear memory.
//
// JS glue (web/unit.js) manages the memory protocol.

#[cfg(target_arch = "wasm32")]
use super::VM;

/// Allocate memory for the JS side to write input strings into.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn alloc(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free allocated memory.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, size: usize) {
    unsafe {
        let _ = Vec::from_raw_parts(ptr, 0, size);
    }
}

/// Boot a new unit VM. Returns a pointer to the heap-allocated VM.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn boot() -> *mut VM {
    let mut vm = VM::new();
    vm.silent = true;
    vm.load_prelude();
    vm.silent = false;
    Box::into_raw(Box::new(vm))
}

/// Evaluate a line of Forth input. Returns a pointer to the output
/// string (NUL-terminated) in WASM memory. The caller must free it.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn eval(vm_ptr: *mut VM, input_ptr: *const u8, input_len: usize) -> *const u8 {
    let vm = unsafe { &mut *vm_ptr };
    let input = unsafe {
        let slice = std::slice::from_raw_parts(input_ptr, input_len);
        String::from_utf8_lossy(slice).to_string()
    };

    // Capture all output.
    vm.output_buffer = Some(String::new());
    vm.interpret_line(&input);
    let output = vm.output_buffer.take().unwrap_or_default();

    // Return NUL-terminated string.
    let mut bytes = output.into_bytes();
    bytes.push(0);
    let ptr = bytes.as_ptr();
    std::mem::forget(bytes);
    ptr
}

/// Check if the VM is still running.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn is_running(vm_ptr: *mut VM) -> i32 {
    let vm = unsafe { &*vm_ptr };
    if vm.running {
        1
    } else {
        0
    }
}

/// Destroy a VM instance.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn destroy(vm_ptr: *mut VM) {
    unsafe {
        let _ = Box::from_raw(vm_ptr);
    }
}
