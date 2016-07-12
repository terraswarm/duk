//! A high-level wrapper around the [Duktape][1] Javascript/EcmaScript
//! interpreter.
//!
//! Currently, the focus is around supporting "extension"/"plug-in"
//! use cases, so the primary supported functionality is:
//!
//!   * Loading code.
//!   * Calling functions and getting their result.
//!
//! Other use-cases (like exposing Rust functions to JS) are not yet
//! implemented.
//!
//! [1]: http://duktape.org/

extern crate duktape_sys;

use std::collections;
use std::ffi;
use std::mem;
use std::os;
use std::path;
use std::ptr;
use std::result;
use std::slice;
use std::str;

/// A context corresponding to a thread of script execution.
pub struct Context(*mut duktape_sys::duk_context);

/// A Javascript/Ecmascript value that has an equivalent Rust mapping.
///
/// Duktape supports values beyond these, but they don't have good
/// Rust semantics, so they cannot be interacted with from the Rust
/// world.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The `undefined` value.
    Undefined,
    /// The `null` value.
    Null,
    /// A boolean like `true` or `false`.
    Boolean(bool),
    /// Any number (both integral like `5` and fractional like `2.3`).
    Number(f64),
    /// Any string like `'abc'`.
    String(String),
    /// Any array of values like `['a', 2, false]`.
    Array(Vec<Value>),
    /// A JSON-like object like `{a: 'a', b: 2, c: false}`.
    Object(collections::BTreeMap<String, Value>),
    /// A Duktape byte buffer like `Duktape.Buffer('abc')`.
    Bytes(Vec<u8>),
}

/// The type of errors that might occur.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// An error that originates from executing Javascript/Ecmascript.
    Js {
        /// The kind of error.
        kind: JsErrorKind,
        /// A descriptive user-controlled error message.
        message: String,
    },
    /// An error that indicates that the specified type has no
    /// equivalent `Value` mapping.
    UnsupportedType(&'static str),
    /// An error that indicates that the specified thing
    /// (function/variable/...) does not exist.
    NonExistent,
}

/// Kinds of Javascript/Ecmascript errors
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsErrorKind {
    /// A thrown error that doesn't inherit from `Error`, like when
    /// the user does `throw 3.14;`.
    Generic,

    /// Duktape internal.
    Unimplemented,
    /// Duktape internal.
    Unsupported,
    /// Duktape internal.
    Internal,
    /// Duktape internal.
    Alloc,
    /// Duktape internal.
    Assertion,
    /// Duktape internal.
    Api,
    /// Duktape internal.
    Uncaught,

    /// An error that's an instance of `Error`.
    Error,
    /// An error that's an instance of `EvalError`.
    Eval,
    /// An error that's an instance of `RangeError`.
    Range,
    /// An error that's an instance of `ReferenceError`.
    Reference,
    /// An error that's an instance of `SyntaxError`.
    Syntax,
    /// An error that's an instance of `TypeError`.
    Type,
    /// An error that's an instance of `UriError`.
    Uri,
}

/// Convenience type for results using the `Error` type.
pub type Result<A> = result::Result<A, Error>;

impl Context {
    /// Creates a new context.
    pub fn new() -> Context {
        let ctx = unsafe {
            duktape_sys::duk_create_heap(None, None, None, ptr::null_mut(), Some(fatal_handler))
        };
        Context(ctx)
    }

    /// Evaluates the specified script string within the current
    /// context.
    ///
    /// # Examples
    ///
    /// Successful evaluation:
    ///
    /// ```
    /// let mut ctx = duk::Context::new();
    /// let value = ctx.eval_string("'ab' + 'cd' + Math.floor(2.3)").unwrap();
    /// assert_eq!(duk::Value::String("abcd2".to_owned()), value);
    /// ```
    ///
    /// However, if we try to call a function that doesn't exist:
    ///
    /// ```
    /// let mut ctx = duk::Context::new();
    /// match ctx.eval_string("var a = {}; a.foo()") {
    ///   Err(duk::Error::Js { kind, message, .. }) => {
    ///     assert_eq!(duk::JsErrorKind::Type, kind);
    ///     assert_eq!("TypeError: undefined not callable", message);
    ///   },
    ///   _ => unreachable!(),
    /// }
    /// ```
    pub fn eval_string(&mut self, string: &str) -> Result<Value> {
        let ptr = string.as_ptr() as *const i8;
        let len = string.len();
        unsafe {
            let ret = duktape_sys::duk_peval_lstring(self.0, ptr, len);
            self.pop_value_or_error(ret)
        }
    }

    /// Loads and evaluates the specified file within the current
    /// context.
    pub fn eval_file(&mut self, path: &path::Path) -> Result<Value> {
        let str_path = path.to_string_lossy();
        let ffi_str = ffi::CString::new(&*str_path).unwrap();
        unsafe {
            let ret = duktape_sys::duk_peval_file(self.0, ffi_str.as_ptr());
            self.pop_value_or_error(ret)
        }
    }

    /// Calls the specified global script function with the supplied
    /// arguments.
    pub fn call_global(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        unsafe {
            duktape_sys::duk_push_global_object(self.0);
            let ffi_name = ffi::CString::new(name).unwrap();
            if 1 == duktape_sys::duk_get_prop_string(self.0, -1, ffi_name.as_ptr()) {
                for arg in args {
                    arg.push(self.0);
                }
                let ret = duktape_sys::duk_pcall(self.0, args.len() as i32);
                let result = self.pop_value_or_error(ret);
                duktape_sys::duk_pop(self.0);
                result
            } else {
                duktape_sys::duk_pop_2(self.0);
                Err(Error::NonExistent)
            }
        }
    }

    #[cfg(test)]
    pub fn assert_clean(&mut self) {
        unsafe {
            assert!(duktape_sys::duk_get_top(self.0) == 0,
                    "context stack is not empty");
        }
    }

    unsafe fn pop_value_or_error(&mut self, ret: duktape_sys::duk_ret_t) -> Result<Value> {
        if ret == 0 {
            let v = try!(Value::get(self.0, -1));
            duktape_sys::duk_pop(self.0);
            Ok(v)
        } else {
            let e = Error::get(self.0, -1);
            duktape_sys::duk_pop(self.0);
            Err(e)
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { duktape_sys::duk_destroy_heap(self.0) };
    }
}

impl Value {
    unsafe fn get(ctx: *mut duktape_sys::duk_context,
                  index: duktape_sys::duk_idx_t)
                  -> Result<Value> {
        let t = duktape_sys::duk_get_type(ctx, index);
        if t == duktape_sys::DUK_TYPE_UNDEFINED {
            Ok(Value::Undefined)
        } else if t == duktape_sys::DUK_TYPE_NULL {
            Ok(Value::Null)
        } else if t == duktape_sys::DUK_TYPE_BOOLEAN {
            Ok(Value::Boolean(duktape_sys::duk_get_boolean(ctx, index) != 0))
        } else if t == duktape_sys::DUK_TYPE_NUMBER {
            Ok(Value::Number(duktape_sys::duk_get_number(ctx, index)))
        } else if t == duktape_sys::DUK_TYPE_STRING {
            Ok(Value::String(get_string(ctx, index)))
        } else if t == duktape_sys::DUK_TYPE_OBJECT {
            if 1 == duktape_sys::duk_is_array(ctx, index) {
                let len = duktape_sys::duk_get_length(ctx, index);
                let mut array = Vec::with_capacity(len);

                for i in 0..len {
                    assert!(1 == duktape_sys::duk_get_prop_index(ctx, index, i as u32));
                    array.push(try!(Value::get(ctx, -1)));
                    duktape_sys::duk_pop(ctx);
                }

                Ok(Value::Array(array))
            } else {
                let mut object = collections::BTreeMap::new();
                duktape_sys::duk_enum(ctx, -1, duktape_sys::DUK_ENUM_OWN_PROPERTIES_ONLY);

                while 1 == duktape_sys::duk_next(ctx, -1, 1) {
                    let key = get_string(ctx, -2);
                    let value = try!(Value::get(ctx, -1));
                    duktape_sys::duk_pop_2(ctx);
                    object.insert(key, value);
                }

                duktape_sys::duk_pop(ctx);

                Ok(Value::Object(object))
            }
        } else if t == duktape_sys::DUK_TYPE_BUFFER {
            let mut size = mem::uninitialized();
            let data = duktape_sys::duk_get_buffer(ctx, index, &mut size);
            let slice = slice::from_raw_parts(data as *const u8, size);
            Ok(Value::Bytes(slice.to_vec()))
        } else if t == duktape_sys::DUK_TYPE_POINTER {
            Err(Error::UnsupportedType("pointer"))
        } else if t == duktape_sys::DUK_TYPE_LIGHTFUNC {
            Err(Error::UnsupportedType("lightfunc"))
        } else {
            panic!("Unmapped type {}", t)
        }
    }

    unsafe fn push(&self, ctx: *mut duktape_sys::duk_context) {
        match *self {
            Value::Undefined => duktape_sys::duk_push_undefined(ctx),
            Value::Null => duktape_sys::duk_push_null(ctx),
            Value::Boolean(b) => {
                let v = if b {
                    1
                } else {
                    0
                };
                duktape_sys::duk_push_boolean(ctx, v);
            }
            Value::Number(n) => duktape_sys::duk_push_number(ctx, n),
            Value::String(ref string) => {
                let data = string.as_ptr() as *const i8;
                let len = string.len();
                duktape_sys::duk_push_lstring(ctx, data, len);
            }
            Value::Array(ref array) => {
                duktape_sys::duk_push_array(ctx);
                for (i, elem) in array.iter().enumerate() {
                    elem.push(ctx);
                    assert!(1 == duktape_sys::duk_put_prop_index(ctx, -2, i as u32));
                }
            }
            Value::Object(ref object) => {
                duktape_sys::duk_push_object(ctx);

                for (k, v) in object {
                    let k_data = k.as_ptr() as *const i8;
                    let k_len = k.len();
                    duktape_sys::duk_push_lstring(ctx, k_data, k_len);
                    v.push(ctx);
                    duktape_sys::duk_put_prop(ctx, -3);
                }
            }
            Value::Bytes(ref bytes) => {
                let len = bytes.len();
                let data = duktape_sys::duk_push_fixed_buffer(ctx, len);

                ptr::copy(bytes.as_ptr(), data as *mut u8, len);
            }
        }
    }
}

impl Error {
    unsafe fn get(ctx: *mut duktape_sys::duk_context, index: duktape_sys::duk_idx_t) -> Error {
        let e = duktape_sys::duk_get_error_code(ctx, index);
        let kind = JsErrorKind::from_raw(e);

        let mut len = mem::uninitialized();
        let data = duktape_sys::duk_safe_to_lstring(ctx, index, &mut len);
        let msg_slice = slice::from_raw_parts(data as *const u8, len);
        let message = String::from(str::from_utf8(msg_slice).unwrap());

        Error::Js {
            kind: kind,
            message: message,
        }
    }
}

impl JsErrorKind {
    unsafe fn from_raw(e: duktape_sys::duk_errcode_t) -> JsErrorKind {
        if e == duktape_sys::DUK_ERR_NONE {
            JsErrorKind::Generic
        } else if e == duktape_sys::DUK_ERR_UNIMPLEMENTED_ERROR {
            JsErrorKind::Unimplemented
        } else if e == duktape_sys::DUK_ERR_UNSUPPORTED_ERROR {
            JsErrorKind::Unsupported
        } else if e == duktape_sys::DUK_ERR_INTERNAL_ERROR {
            JsErrorKind::Internal
        } else if e == duktape_sys::DUK_ERR_ALLOC_ERROR {
            JsErrorKind::Alloc
        } else if e == duktape_sys::DUK_ERR_ASSERTION_ERROR {
            JsErrorKind::Assertion
        } else if e == duktape_sys::DUK_ERR_API_ERROR {
            JsErrorKind::Api
        } else if e == duktape_sys::DUK_ERR_UNCAUGHT_ERROR {
            JsErrorKind::Uncaught
        } else if e == duktape_sys::DUK_ERR_ERROR {
            JsErrorKind::Error
        } else if e == duktape_sys::DUK_ERR_EVAL_ERROR {
            JsErrorKind::Eval
        } else if e == duktape_sys::DUK_ERR_RANGE_ERROR {
            JsErrorKind::Range
        } else if e == duktape_sys::DUK_ERR_REFERENCE_ERROR {
            JsErrorKind::Reference
        } else if e == duktape_sys::DUK_ERR_SYNTAX_ERROR {
            JsErrorKind::Syntax
        } else if e == duktape_sys::DUK_ERR_TYPE_ERROR {
            JsErrorKind::Type
        } else if e == duktape_sys::DUK_ERR_URI_ERROR {
            JsErrorKind::Uri
        } else {
            panic!("Unmapped error code {}", e)
        }
    }
}

unsafe fn get_string(ctx: *mut duktape_sys::duk_context, index: duktape_sys::duk_idx_t) -> String {
    let mut len = mem::uninitialized();
    let data = duktape_sys::duk_get_lstring(ctx, index, &mut len);
    let slice = slice::from_raw_parts(data as *const u8, len);
    String::from(str::from_utf8(slice).unwrap())
}

unsafe extern "C" fn fatal_handler(ctx: *mut duktape_sys::duk_context,
                                   code: duktape_sys::duk_errcode_t,
                                   msg_raw: *const os::raw::c_char) {
    let msg = &*ffi::CStr::from_ptr(msg_raw).to_string_lossy();
    duktape_sys::duk_push_context_dump(ctx);
    let context_dump = get_string(ctx, -1);
    duktape_sys::duk_pop(ctx);
    // TODO: No unwind support from C... but this "works" right now
    panic!("Duktape fatal error (code {}): {}\n{}",
           code,
           msg,
           context_dump)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections;

    #[test]
    fn eval_string_undefined() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("undefined");
        assert_eq!(Ok(Value::Undefined), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_null() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("null");
        assert_eq!(Ok(Value::Null), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_boolean_true() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("true");
        assert_eq!(Ok(Value::Boolean(true)), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_boolean_false() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("false");
        assert_eq!(Ok(Value::Boolean(false)), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_number_integral() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("4");
        assert_eq!(Ok(Value::Number(4.0)), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_number_fractional() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("0.5");
        assert_eq!(Ok(Value::Number(0.5)), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_string() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("'ab'");
        assert_eq!(Ok(Value::String("ab".to_owned())), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_array() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("['a', 3, false]");
        assert_eq!(Ok(Value::Array(vec![Value::String("a".to_owned()),
                                        Value::Number(3.0),
                                        Value::Boolean(false)])),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_object() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("({a: 'a', b: 3, c: false})");

        let mut expected = collections::BTreeMap::new();
        expected.insert("a".to_owned(), Value::String("a".to_owned()));
        expected.insert("b".to_owned(), Value::Number(3.0));
        expected.insert("c".to_owned(), Value::Boolean(false));

        assert_eq!(Ok(Value::Object(expected)), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_buffer() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("Duktape.Buffer('abc')");
        assert_eq!(Ok(Value::Bytes("abc".as_bytes().to_vec())), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_error_generic() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw 'foobar';");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Generic,
                       message: "foobar".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_error_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new Error('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Error,
                       message: "Error: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_eval_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new EvalError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Eval,
                       message: "EvalError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_range_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new RangeError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Range,
                       message: "RangeError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_reference_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new ReferenceError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Reference,
                       message: "ReferenceError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_syntax_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new SyntaxError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Syntax,
                       message: "SyntaxError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_type_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new TypeError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Type,
                       message: "TypeError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_uri_error() {
        let mut ctx = Context::new();
        let value = ctx.eval_string("throw new URIError('xyz')");
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Uri,
                       message: "URIError: xyz".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_call_global() {
        let mut ctx = Context::new();
        ctx.eval_string(r"
          function foo() {
            return 'a';
          }")
           .unwrap();
        let value = ctx.call_global("foo", &[]);
        assert_eq!(Ok(Value::String("a".to_owned())), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_call_global_args() {
        let mut ctx = Context::new();
        ctx.eval_string(r"
          function foo() {
            return Array.prototype.slice.call(arguments);
          }")
           .unwrap();

        let mut obj = collections::BTreeMap::new();
        obj.insert("a".to_owned(), Value::String("a".to_owned()));
        obj.insert("b".to_owned(), Value::Number(3.0));
        obj.insert("c".to_owned(), Value::Boolean(false));

        let arr = vec![Value::String("a".to_owned()), Value::Number(3.0), Value::Boolean(false)];

        let bytes = vec![0, 1, 2, 3];

        let args = &[Value::Undefined,
                     Value::Null,
                     Value::Boolean(true),
                     Value::Number(1.0),
                     Value::String("foo".to_owned()),
                     Value::Array(arr),
                     Value::Object(obj),
                     Value::Bytes(bytes)];
        let value = ctx.call_global("foo", args);
        assert_eq!(Ok(Value::Array(args.to_vec())), value);
        ctx.assert_clean();
    }

    #[test]
    fn eval_string_call_global_error() {
        let mut ctx = Context::new();
        ctx.eval_string(r"
          function foo() {
            throw 'a';
          }")
           .unwrap();
        let value = ctx.call_global("foo", &[]);
        assert_eq!(Err(Error::Js {
                       kind: JsErrorKind::Generic,
                       message: "a".to_owned(),
                   }),
                   value);
        ctx.assert_clean();
    }

    #[test]
    fn call_non_existent() {
        let mut ctx = Context::new();
        let value = ctx.call_global("foo", &[]);
        assert_eq!(Err(Error::NonExistent), value);
        ctx.assert_clean();
    }
}
