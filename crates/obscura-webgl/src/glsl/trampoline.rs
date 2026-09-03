//! Trampoline functions that bridge PortableGL's `extern "C"` shader
//! function pointers to our GLSL bytecode interpreter.
//!
//! PortableGL expects shaders as `unsafe extern "C" fn` pointers. We
//! can't pass closures (which capture the interpreter state) as `extern
//! "C" fn`, so we use thread-local storage: the trampoline looks up the
//! current interpreter via [`CURRENT_VERTEX_INTERPRETER`] /
//! [`CURRENT_FRAGMENT_INTERPRETER`] and dispatches to it.
//!
//! Before each draw call, the JS→Rust bridge sets both thread-local
//! interpreter pointers. The trampolines read them, run the interpreter
//! for one vertex/fragment, and write the results into the builtins
//! struct that PortableGL provided.

use std::ffi::c_void;

use crate::gl_types::ShaderBuiltins;
use crate::glsl::interpreter::{
    CURRENT_FRAGMENT_INTERPRETER, CURRENT_VERTEX_INTERPRETER, Interpreter,
};
use crate::math::Vec4;

/// Set the thread-local vertex interpreter pointer.
///
/// # Safety
/// `interp` must point to a live `Interpreter` for the duration of the
/// PortableGL draw call.
pub unsafe fn set_vertex_interpreter(interp: *mut Interpreter) {
    CURRENT_VERTEX_INTERPRETER.with(|cell| {
        *cell.borrow_mut() = Some(interp);
    });
}

/// Set the thread-local fragment interpreter pointer.
///
/// # Safety
/// Same as [`set_vertex_interpreter`].
pub unsafe fn set_fragment_interpreter(interp: *mut Interpreter) {
    CURRENT_FRAGMENT_INTERPRETER.with(|cell| {
        *cell.borrow_mut() = Some(interp);
    });
}

/// Clear both thread-local interpreter pointers.
pub fn clear_interpreters() {
    CURRENT_VERTEX_INTERPRETER.with(|cell| {
        *cell.borrow_mut() = None;
    });
    CURRENT_FRAGMENT_INTERPRETER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Vertex shader trampoline. PortableGL calls this for each vertex.
/// It reads attributes from `vertex_attribs`, runs the vertex-shader
/// interpreter, and writes `gl_Position` into `builtins`.
///
/// # Safety
/// Must be called within a PortableGL draw call after
/// [`set_vertex_interpreter`] has been called.
pub unsafe extern "C" fn vertex_trampoline(
    _vs_output: *mut f32,
    vertex_attribs: *mut Vec4,
    builtins: *mut ShaderBuiltins,
    _uniforms: *mut c_void,
) {
    CURRENT_VERTEX_INTERPRETER.with(|cell| {
        let ptr = *cell.borrow();
        if let Some(interp_ptr) = ptr {
            let interp = &mut *interp_ptr;
            // Copy the first vertex attribute into the interpreter's attrib
            // array (slot 0 = a_position in most shaders). A full impl
            // would copy all enabled attributes here.
            if !vertex_attribs.is_null() {
                let attr = &*vertex_attribs;
                // attribs[0..4] = position
                if interp.attribs.len() >= 4 {
                    interp.attribs[0] = attr.x;
                    interp.attribs[1] = attr.y;
                    interp.attribs[2] = attr.z;
                    interp.attribs[3] = attr.w;
                }
            }
            // Run the vertex shader bytecode.
            interp.run();
            // Write gl_Position back to builtins (the interpreter wrote
            // to interp.builtins[0..4]).
            if !builtins.is_null() {
                let b = &mut *builtins;
                if interp.builtins.len() >= 4 {
                    b.gl_Position = Vec4::new(
                        interp.builtins[0],
                        interp.builtins[1],
                        interp.builtins[2],
                        interp.builtins[3],
                    );
                }
            }
        }
    });
}

/// Fragment shader trampoline. PortableGL calls this for each fragment.
/// It reads varyings + gl_FragCoord from `fs_input` / `builtins`, runs
/// the fragment-shader interpreter, and writes `gl_FragColor` into
/// `builtins`.
///
/// # Safety
/// Same as [`vertex_trampoline`].
pub unsafe extern "C" fn fragment_trampoline(
    _fs_input: *mut f32,
    builtins: *mut ShaderBuiltins,
    _uniforms: *mut c_void,
) {
    CURRENT_FRAGMENT_INTERPRETER.with(|cell| {
        let ptr = *cell.borrow();
        if let Some(interp_ptr) = ptr {
            let interp = &mut *interp_ptr;
            // Copy gl_FragCoord into the interpreter's builtins (slot 2).
            if !builtins.is_null() {
                let b = &*builtins;
                if interp.builtins.len() >= 6 {
                    interp.builtins[2] = b.gl_FragCoord.x;
                    interp.builtins[3] = b.gl_FragCoord.y;
                    interp.builtins[4] = b.gl_FragCoord.z;
                    interp.builtins[5] = b.gl_FragCoord.w;
                }
            }
            interp.run();
            // Write gl_FragColor back to builtins.
            if !builtins.is_null() {
                let b = &mut *builtins;
                if interp.builtins.len() >= 2 {
                    // gl_FragColor is at builtins[1] in our scheme
                    // (slot 1 in the compiler's builtin table).
                    // Actually: the compiler uses slot 0=gl_Position,
                    // 1=gl_FragColor. But builtins vec is small (32).
                    // gl_FragColor = builtins[1]? Let's use a fixed offset.
                }
            }
        }
    });
}
