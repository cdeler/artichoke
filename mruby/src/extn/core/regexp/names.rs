use std::cmp::Ordering;

use crate::convert::{FromMrb, RustBackedValue};
use crate::extn::core::regexp::Regexp;
use crate::interpreter::Mrb;
use crate::value::Value;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum Error {
    Fatal,
}

pub fn method(interp: &Mrb, value: &Value) -> Result<Value, Error> {
    let data = unsafe { Regexp::try_from_ruby(interp, value) }.map_err(|_| Error::Fatal)?;
    let borrow = data.borrow();
    let mut names = vec![];
    let mut capture_names = borrow.regex.capture_names().collect::<Vec<_>>();
    capture_names.sort_by(|a, b| {
        a.1.iter()
            .fold(u32::max_value(), |a, &b| a.min(b))
            .partial_cmp(b.1.iter().fold(&u32::max_value(), |a, b| a.min(b)))
            .unwrap_or(Ordering::Equal)
    });
    for (name, _) in capture_names {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(Value::from_mrb(&interp, names))
}
