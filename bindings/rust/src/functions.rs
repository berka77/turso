//! Application-defined collations and scalar functions for a connection.
//!
//! The engine calls them through its extension interface (C calling convention, a context
//! pointer and raw values), so a Rust closure is boxed into the context and reached through a
//! small trampoline. These wrappers keep all of that out of the caller's code.

use std::cmp::Ordering;
use std::panic::{catch_unwind, AssertUnwindSafe};

use turso_sdk_kit::rsapi::ExtensionValue;

use crate::connection::Connection;
use crate::{Error, Result, Value};

type CollationFn = dyn Fn(&str, &str) -> Ordering + Send + Sync + 'static;
type ScalarFn = dyn Fn(&[Value]) -> std::result::Result<Value, String> + Send + Sync + 'static;

impl Connection {
    /// Register a collation under `name` for this connection, for use as `COLLATE name` in
    /// queries (`ORDER BY`, comparisons). It must be a total order. Connection-local collations
    /// cannot appear in table or index definitions, which every connection shares; use a locale
    /// collation (`COLLATE "sv-u-ks-level1"`) there. Registering the same name again replaces
    /// the earlier collation.
    pub fn register_collation<F>(&self, name: &str, compare: F) -> Result<()>
    where
        F: Fn(&str, &str) -> Ordering + Send + Sync + 'static,
    {
        let boxed: Box<Box<CollationFn>> = Box::new(Box::new(compare));
        let context = Box::into_raw(boxed) as usize;
        self.get_inner_connection()?.register_external_collation(
            name.to_string(),
            context,
            collation_trampoline,
            Some(drop_collation),
        );
        Ok(())
    }

    /// Register a scalar SQL function `name` taking `argc` arguments (-1: any number) for this
    /// connection. An `Err` from the function fails the statement with that message; a panic is
    /// caught and reported the same way. `deterministic` functions may be used in indexes and
    /// generated columns and may be evaluated fewer times than they appear.
    pub fn register_scalar_function<F>(
        &self,
        name: &str,
        argc: i32,
        deterministic: bool,
        func: F,
    ) -> Result<()>
    where
        F: Fn(&[Value]) -> std::result::Result<Value, String> + Send + Sync + 'static,
    {
        let boxed: Box<Box<ScalarFn>> = Box::new(Box::new(func));
        let context = Box::into_raw(boxed) as usize;
        let registered = self
            .get_inner_connection()?
            .register_external_scalar_function(
                name.to_string(),
                argc,
                deterministic,
                context,
                scalar_trampoline,
                Some(drop_scalar),
                None,
            );
        registered.map_err(Error::from)
    }
}

unsafe extern "C" fn collation_trampoline(
    context: usize,
    left_ptr: *const u8,
    left_len: usize,
    right_ptr: *const u8,
    right_len: usize,
) -> i32 {
    let compare = unsafe { &*(context as *const Box<CollationFn>) };
    // The engine passes text it validated as UTF-8.
    let text = |ptr: *const u8, len: usize| unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len))
    };
    let (left, right) = (text(left_ptr, left_len), text(right_ptr, right_len));
    // A panic must not unwind into the engine; fall back to byte order.
    match catch_unwind(AssertUnwindSafe(|| compare(left, right)))
        .unwrap_or_else(|_| left.cmp(right))
    {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

unsafe extern "C" fn drop_collation(context: usize) {
    drop(unsafe { Box::from_raw(context as *mut Box<CollationFn>) });
}

unsafe extern "C" fn scalar_trampoline(
    context: usize,
    argc: i32,
    argv: *const ExtensionValue,
    _context_destructor: Option<turso_sdk_kit::rsapi::ContextDestructor>,
    _value_destructor: Option<turso_sdk_kit::rsapi::ValueDestructor>,
) -> ExtensionValue {
    let func = unsafe { &*(context as *const Box<ScalarFn>) };
    let raw = if argc <= 0 || argv.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(argv, argc as usize) }
    };
    let args: Vec<Value> = raw.iter().map(from_extension_value).collect();
    match catch_unwind(AssertUnwindSafe(|| func(&args))) {
        Ok(Ok(value)) => to_extension_value(value),
        Ok(Err(message)) => ExtensionValue::error_with_message(message),
        Err(_) => ExtensionValue::error_with_message("function panicked".to_string()),
    }
}

unsafe extern "C" fn drop_scalar(context: usize) {
    drop(unsafe { Box::from_raw(context as *mut Box<ScalarFn>) });
}

fn from_extension_value(v: &ExtensionValue) -> Value {
    use turso_sdk_kit::rsapi::ExtensionValueType as T;
    match v.value_type() {
        T::Integer => v.to_integer().map_or(Value::Null, Value::Integer),
        T::Float => v.to_float().map_or(Value::Null, Value::Real),
        T::Text => v
            .to_text()
            .map_or(Value::Null, |s| Value::Text(s.to_string())),
        T::Blob => v.to_blob().map_or(Value::Null, Value::Blob),
        T::Null | T::Error => Value::Null,
    }
}

fn to_extension_value(v: Value) -> ExtensionValue {
    match v {
        Value::Null => ExtensionValue::null(),
        Value::Integer(i) => ExtensionValue::from_integer(i),
        Value::Real(f) => ExtensionValue::from_float(f),
        Value::Text(s) => ExtensionValue::from_text(s),
        Value::Blob(b) => ExtensionValue::from_blob(b),
    }
}
