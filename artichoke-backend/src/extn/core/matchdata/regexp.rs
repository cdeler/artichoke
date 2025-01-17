//! [`MatchData#regexp`](https://ruby-doc.org/core-2.6.3/MatchData.html#method-i-regexp)

use crate::convert::RustBackedValue;
use crate::extn::core::matchdata::MatchData;
use crate::value::Value;
use crate::Artichoke;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum Error {
    Fatal,
}

pub fn method(interp: &Artichoke, value: &Value) -> Result<Value, Error> {
    if let Ok(data) = unsafe { MatchData::try_from_ruby(interp, value) } {
        let borrow = data.borrow();
        let regexp = borrow.regexp.clone();
        unsafe { regexp.try_into_ruby(interp, None) }.map_err(|_| Error::Fatal)
    } else {
        Err(Error::Fatal)
    }
}
