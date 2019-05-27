use log::{debug, error, trace};
use mruby_vfs::FileSystem;
use std::cell::RefCell;
use std::convert::AsRef;
use std::ffi::{c_void, CStr, CString};
use std::mem;
use std::path::PathBuf;
use std::rc::Rc;

use crate::class;
use crate::convert::{Float, FromMrb, Int};
use crate::def::{ClassLike, Define};
use crate::eval::{EvalContext, MrbEval};
use crate::gc::GarbageCollection;
use crate::module;
use crate::state::{State, VfsMetadata};
use crate::sys::{self, DescribeState};
use crate::value::{Value, ValueLike};
use crate::MrbError;

pub const RUBY_LOAD_PATH: &str = "/src/lib";

pub type Mrb = Rc<RefCell<State>>;

fn raise_load_error(interp: &Mrb, file: &str) -> sys::mrb_value {
    let eclass = CString::new("LoadError").expect("RuntimeError class");
    let message = format!("cannot load such file -- {}", file);
    let msg = CString::new(message).expect("error message");
    unsafe { sys::mrb_sys_raise(interp.borrow().mrb, eclass.as_ptr(), msg.as_ptr()) };
    debug!("Failed require '{}' on {:?}", file, interp.borrow());
    interp.bool(false).inner()
}

extern "C" fn require(mrb: *mut sys::mrb_state, _slf: sys::mrb_value) -> sys::mrb_value {
    let interp = unsafe { interpreter_or_raise!(mrb) };
    // Extract required filename from arguments
    let name = unsafe {
        let name = mem::uninitialized::<*const std::os::raw::c_char>();
        let argspec = CString::new(sys::specifiers::CSTRING).expect("argspec");
        sys::mrb_get_args(mrb, argspec.as_ptr(), &name);
        match CStr::from_ptr(name).to_str() {
            Ok(name) => name.to_owned(),
            Err(err) => {
                let eclass = CString::new("ArgumentError");
                let message = CString::new(format!("{}", err));
                if let (Ok(eclass), Ok(message)) = (eclass, message) {
                    sys::mrb_sys_raise(interp.borrow().mrb, eclass.as_ptr(), message.as_ptr());
                }
                return interp.nil().inner();
            }
        }
    };

    // track whether any iterations of the loop successfully required a file
    let mut success = false;
    let mut path = PathBuf::from(&name);
    if path.is_relative() {
        path = PathBuf::from(RUBY_LOAD_PATH);
    }
    let files = vec![path.join(&name), path.join(format!("{}.rb", name))];
    for path in files {
        let is_file = {
            let api = interp.borrow();
            api.vfs.is_file(&path)
        };
        if !is_file {
            // If no paths are files in the VFS, then the require does
            // nothing.
            continue;
        }
        let metadata = {
            let api = interp.borrow();
            api.vfs.metadata(&path).unwrap_or_else(VfsMetadata::new)
        };
        // If a file is already required, short circuit
        if metadata.is_already_required() {
            return interp.bool(false).inner();
        }
        let context = if let Some(filename) = &path.to_str() {
            EvalContext::new(filename)
        } else {
            EvalContext::new("(require)")
        };
        // Require Rust MrbFile first because an MrbFile may define classes and
        // module with `MrbLoadSources` and Ruby files can require arbitrary
        // other files, including some child sources that may depend on these
        // module definitions. This behavior is enforced with a test in crate
        // mruby-gems. See mruby-gems/src/lib.rs.
        if let Some(require) = metadata.require {
            // dynamic, Rust-backed `MrbFile` require
            interp.push_context(context.clone());
            unsafe { unwrap_or_raise!(interp, require(Rc::clone(&interp)), interp.nil().inner()) };
            interp.pop_context();
        }
        let contents = {
            let api = interp.borrow();
            api.vfs.read_file(&path)
        };
        if let Ok(contents) = contents {
            unsafe {
                unwrap_value_or_raise!(interp, interp.eval_with_context(contents, context));
            }
        } else {
            // this branch should be unreachable because the `Mrb` interpreter
            // is not `Send` so it can only be owned and accessed by one thread.
            return raise_load_error(&interp, &name);
        }
        let metadata = metadata.mark_required();
        unsafe {
            let api = interp.borrow();
            unwrap_or_raise!(
                interp,
                api.vfs.set_metadata(&path, metadata),
                interp.nil().inner()
            );
        }
        success = true;
        trace!(
            r#"Successful require of "{}" at {:?} on {:?}"#,
            name,
            path,
            interp.borrow()
        );
    }
    if success {
        interp.bool(success).inner()
    } else {
        raise_load_error(&interp, &name)
    }
}

pub struct Interpreter;

impl Interpreter {
    pub fn create() -> Result<Mrb, MrbError> {
        unsafe {
            let mrb = sys::mrb_open();
            if mrb.is_null() {
                error!("Failed to allocate mrb interprter");
                return Err(MrbError::New);
            }

            let context = sys::mrbc_context_new(mrb);
            let api = Rc::new(RefCell::new(State::new(mrb, context, RUBY_LOAD_PATH)));

            // Transmute the smart pointer that wraps the API and store it in
            // the user data of the mrb interpreter. After this operation,
            // `Rc::strong_count` will still be 1.
            let ptr = mem::transmute::<Mrb, *mut c_void>(api);
            (*mrb).ud = ptr;

            // Transmute the void * pointer to the Rc back into the Mrb type.
            // After this operation `Rc::strong_count` will still be 1. This
            // dance is required to avoid leaking Mrb objects, which will let
            // the `Drop` impl close the mrb context and interpreter.
            let interp = mem::transmute::<*mut c_void, Mrb>(ptr);

            // Add global extension functions
            // Support for requiring files via `Kernel#require`
            // TODO: clean this up by making a spec factory
            let mut kernel = module::Spec::new("Kernel", None);
            kernel.add_self_method("require", require, sys::mrb_args_rest());
            kernel.define(&interp).map_err(|_| MrbError::New)?;
            trace!("Installed Kernel#require on {}", mrb.debug());
            let exception = class::Spec::new("Exception", None, None);
            let exception_rc = Rc::new(RefCell::new(exception));
            let mut script_error = class::Spec::new("ScriptError", None, None);
            script_error.with_super_class(Rc::clone(&exception_rc));
            script_error.define(&interp).map_err(|_| MrbError::New)?;
            let script_error_rc = Rc::new(RefCell::new(script_error));
            let mut load_error = class::Spec::new("LoadError", None, None);
            load_error.with_super_class(Rc::clone(&script_error_rc));
            load_error.define(&interp).map_err(|_| MrbError::New)?;
            trace!("Installed LoadError on {}", mrb.debug());

            debug!("Allocated {}", mrb.debug());

            // mruby lazily initializes some core objects like top_self and
            // generates a lot of garbage on startup. Eagerly initialize the
            // interpreter to provide predictable initialization behavior.
            let arena = interp.create_arena_savepoint();
            interp.eval("").map_err(|_| MrbError::New)?;
            arena.restore();
            interp.full_gc();
            Ok(interp)
        }
    }

    // TODO: Add a benchmark to make sure this function does not leak memory.
    pub unsafe fn from_user_data(mrb: *mut sys::mrb_state) -> Result<Mrb, MrbError> {
        if mrb.is_null() {
            error!("Attempted to extract Mrb from null mrb_state");
            return Err(MrbError::Uninitialized);
        }
        let ptr = (*mrb).ud;
        if ptr.is_null() {
            error!("Attempted to extract Mrb from null mrb_state->ud pointer");
            return Err(MrbError::Uninitialized);
        }
        // Extract the smart pointer that wraps the API from the user data on
        // the mrb interpreter. The `mrb_state` should retain ownership of its
        // copy of the smart pointer.
        let ud = mem::transmute::<*mut c_void, Mrb>(ptr);
        // Clone the API smart pointer and increase its ref count to return a
        // reference to the caller.
        let api = Rc::clone(&ud);
        // Forget the transmuted API extracted from the user data to make sure
        // the `mrb_state` maintains ownership and the smart pointer does not
        // get deallocated before `mrb_close` is called.
        mem::forget(ud);
        // At this point, `Rc::strong_count` will be increased by 1.
        trace!("Extracted Mrb from user data pointer on {}", mrb.debug());
        Ok(api)
    }
}

pub trait MrbApi {
    fn current_exception(&self) -> Option<String>;

    fn nil(&self) -> Value;

    fn bool(&self, b: bool) -> Value;

    fn bytes<T: AsRef<[u8]>>(&self, b: T) -> Value;

    fn fixnum(&self, i: Int) -> Value;

    fn float(&self, i: Float) -> Value;

    fn string<T: AsRef<str>>(&self, s: T) -> Value;
}

impl MrbApi for Mrb {
    /// Extract a `String` representation of the current exception on the mruby
    /// interpreter if there is one. The string will contain the exception
    /// class, message, and backtrace.
    fn current_exception(&self) -> Option<String> {
        let _arena = self.create_arena_savepoint();
        let mrb = { self.borrow().mrb };
        let exc = unsafe {
            let exc = (*mrb).exc;
            // Clear the current exception from the mruby interpreter so
            // subsequent calls to the mruby VM are not tainted by an error they
            // did not generate.
            //
            // We must do this at the beginning of `current_exception` so we can
            // use the mruby VM to inspect the exception once we turn it into an
            // `mrb_value`. `ValueLike::funcall` handles errors by calling this
            // function, so not clearing the exception results in a stack
            // overflow.
            (*mrb).exc = std::ptr::null_mut();
            exc
        };
        if exc.is_null() {
            trace!("Last eval had no runtime errors: mrb_state has no current exception");
            return None;
        }
        // Generate an exception backtrace in a `String` by executing the
        // following Ruby code with the C API:
        //
        // ```ruby
        // exception = exc.inspect
        // backtrace = exc.backtrace
        // backtrace.unshift(exception)
        // backtrace.join("\n")
        // ```
        let value = Value::new(self, unsafe { sys::mrb_sys_obj_value(exc as *mut c_void) });
        let exception = value.funcall::<Value, _, _>("inspect", &[]).ok()?;
        let backtrace = value.funcall::<Value, _, _>("backtrace", &[]).ok()?;
        backtrace
            .funcall::<(), _, _>("unshift", &[exception])
            .ok()?;

        backtrace
            .funcall::<String, _, _>("join", &[Value::from_mrb(self, "\n")])
            .ok()
    }

    fn nil(&self) -> Value {
        Value::from_mrb(self, None::<Value>)
    }

    fn bool(&self, b: bool) -> Value {
        Value::from_mrb(self, b)
    }

    fn bytes<T: AsRef<[u8]>>(&self, b: T) -> Value {
        Value::from_mrb(self, b.as_ref())
    }

    fn fixnum(&self, i: Int) -> Value {
        Value::from_mrb(self, i)
    }

    fn float(&self, f: Float) -> Value {
        Value::from_mrb(self, f)
    }

    fn string<T: AsRef<str>>(&self, s: T) -> Value {
        Value::from_mrb(self, s.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use crate::convert::TryFromMrb;
    use crate::eval::MrbEval;
    use crate::file::MrbFile;
    use crate::interpreter::{Interpreter, Mrb, MrbError};
    use crate::load::MrbLoadSources;
    use crate::sys;

    #[test]
    fn from_user_data_null_pointer() {
        unsafe {
            let err = Interpreter::from_user_data(std::ptr::null_mut());
            assert_eq!(err.err(), Some(MrbError::Uninitialized));
        }
    }

    #[test]
    fn from_user_data_null_user_data() {
        unsafe {
            let interp = Interpreter::create().expect("mrb init");
            let api = interp.borrow();
            let mrb = api.mrb;
            // fake null user data
            (*mrb).ud = std::ptr::null_mut();
            let err = Interpreter::from_user_data(mrb);
            assert_eq!(err.err(), Some(MrbError::Uninitialized));
        }
    }

    #[test]
    fn from_user_data() {
        unsafe {
            let interp = Interpreter::create().expect("mrb init");
            let mrb = interp.borrow().mrb;
            let res = Interpreter::from_user_data(mrb);
            assert!(res.is_ok());
        }
    }

    #[test]
    fn open_close() {
        let interp = Interpreter::create().expect("mrb init");
        drop(interp);
    }

    #[test]
    fn load_code() {
        unsafe {
            let interp = Interpreter::create().expect("mrb init");
            let result = interp.eval("255").expect("eval");
            assert_eq!(sys::mrb_sys_fixnum_to_cint(result.inner()), 255);
        }
    }

    #[test]
    fn return_raised_exception() {
        let interp = Interpreter::create().expect("mrb init");
        let result = interp
            .eval("raise ArgumentError.new('waffles')")
            .map(|_| ());
        let expected = r#"
(eval):1: waffles (ArgumentError)
(eval):1
       "#;
        assert_eq!(result, Err(MrbError::Exec(expected.trim().to_owned())));
    }

    #[test]
    // Test that require behaves as expected:
    // - require side effects (e.g. ivar set or class def) affect the interpreter
    // - Successful first require returns `true`.
    // - Second require returns `false`.
    // - Second require does not cause require side effects.
    // - Require non-existing file raises and returns `nil`.
    fn require() {
        struct InterpreterRequireTest;
        impl MrbFile for InterpreterRequireTest {
            fn require(interp: Mrb) -> Result<(), MrbError> {
                interp.eval("@i = 255")?;
                Ok(())
            }
        }

        unsafe {
            let interp = Interpreter::create().expect("mrb init");
            interp
                .def_file_for_type::<_, InterpreterRequireTest>("require-test.rb")
                .expect("def file");
            let result = interp.eval("require 'require-test'").expect("eval");
            let require_result = bool::try_from_mrb(&interp, result);
            assert_eq!(require_result, Ok(true));
            let result = interp.eval("@i").expect("eval");
            let i_result = i64::try_from_mrb(&interp, result);
            assert_eq!(i_result, Ok(255));
            let result = interp
                .eval("@i = 1000; require 'require-test'")
                .expect("eval");
            let second_require_result = bool::try_from_mrb(&interp, result);
            assert_eq!(second_require_result, Ok(false));
            let result = interp.eval("@i").expect("eval");
            let second_i_result = i64::try_from_mrb(&interp, result);
            assert_eq!(second_i_result, Ok(1000));
            let result = interp.eval("require 'non-existent-source'").map(|_| ());
            let expected = r#"
(eval):1: cannot load such file -- non-existent-source (LoadError)
(eval):1
            "#;
            assert_eq!(result, Err(MrbError::Exec(expected.trim().to_owned())));
        }
    }

    #[test]
    fn require_absolute_path() {
        let interp = Interpreter::create().expect("mrb init");
        interp
            .def_rb_source_file("/foo/bar/source.rb", "# a source file")
            .expect("def file");
        let result = interp.eval("require '/foo/bar/source.rb'").expect("value");
        assert!(unsafe { bool::try_from_mrb(&interp, result).expect("convert") });
        let result = interp.eval("require '/foo/bar/source.rb'").expect("value");
        assert!(!unsafe { bool::try_from_mrb(&interp, result).expect("convert") });
    }

    #[test]
    fn require_directory() {
        let interp = Interpreter::create().expect("mrb init");
        let result = interp.eval("require '/src'").map(|_| ());
        let expected = r#"
(eval):1: cannot load such file -- /src (LoadError)
(eval):1
        "#;
        assert_eq!(result, Err(MrbError::Exec(expected.trim().to_owned())));
    }

    #[test]
    fn require_path_defined_as_source_then_mrbfile() {
        struct Foo;
        impl MrbFile for Foo {
            fn require(interp: Mrb) -> Result<(), MrbError> {
                interp.eval("module Foo; RUST = 7; end")?;
                Ok(())
            }
        }
        let interp = Interpreter::create().expect("mrb init");
        interp
            .def_rb_source_file("foo.rb", "module Foo; RUBY = 3; end")
            .expect("def");
        interp.def_file_for_type::<_, Foo>("foo.rb").expect("def");
        let result = interp.eval("require 'foo'").expect("eval");
        let result = unsafe { bool::try_from_mrb(&interp, result).expect("convert") };
        assert!(result, "successfully required foo.rb");
        let result = interp.eval("Foo::RUBY + Foo::RUST").expect("eval");
        let result = unsafe { i64::try_from_mrb(&interp, result).expect("convert") };
        assert_eq!(
            result, 10,
            "defined Ruby and Rust sources from single require"
        );
    }

    #[test]
    fn require_path_defined_as_mrbfile_then_source() {
        struct Foo;
        impl MrbFile for Foo {
            fn require(interp: Mrb) -> Result<(), MrbError> {
                interp.eval("module Foo; RUST = 7; end")?;
                Ok(())
            }
        }
        let interp = Interpreter::create().expect("mrb init");
        interp.def_file_for_type::<_, Foo>("foo.rb").expect("def");
        interp
            .def_rb_source_file("foo.rb", "module Foo; RUBY = 3; end")
            .expect("def");
        let result = interp.eval("require 'foo'").expect("eval");
        let result = unsafe { bool::try_from_mrb(&interp, result).expect("convert") };
        assert!(result, "successfully required foo.rb");
        let result = interp.eval("Foo::RUBY + Foo::RUST").expect("eval");
        let result = unsafe { i64::try_from_mrb(&interp, result).expect("convert") };
        assert_eq!(
            result, 10,
            "defined Ruby and Rust sources from single require"
        );
    }
}
