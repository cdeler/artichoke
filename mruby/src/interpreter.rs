use mruby_sys::*;
use std::convert::AsRef;

use crate::convert::*;
use crate::value::*;

pub struct Mrb {
    mrb: Option<*mut mrb_state>,
}

pub enum MrbError {
    Closed,
    Convert(Error<Rust, Ruby>),
    New,
}

impl Drop for Mrb {
    fn drop(&mut self) {
        if let Some(mrb) = self.mrb.take() {
            unsafe { mrb_close(mrb) };
        }
    }
}

impl Mrb {
    pub fn new() -> Result<Self, MrbError> {
        let mrb = unsafe { mrb_open() };
        let null = std::ptr::null::<mrb_state>() as *mut mrb_state;
        if mrb == null {
            Err(MrbError::New)
        } else {
            Ok(Self { mrb: Some(mrb) })
        }
    }

    pub fn close(self) {
        drop(self)
    }

    pub fn bool(&self, b: bool) -> Result<Value, MrbError> {
        if let Some(mrb) = self.mrb {
            Value::try_from_mrb(mrb, b).map_err(MrbError::Convert)
        } else {
            Err(MrbError::Closed)
        }
    }

    pub fn bytes<T: AsRef<[u8]>>(&self, b: T) -> Result<Value, MrbError> {
        if let Some(mrb) = self.mrb {
            Value::try_from_mrb(mrb, b.as_ref()).map_err(MrbError::Convert)
        } else {
            Err(MrbError::Closed)
        }
    }

    pub fn fixnum(&self, i: Int) -> Result<Value, MrbError> {
        if let Some(mrb) = self.mrb {
            Value::try_from_mrb(mrb, i).map_err(MrbError::Convert)
        } else {
            Err(MrbError::Closed)
        }
    }

    pub fn string<T: AsRef<str>>(&self, s: T) -> Result<Value, MrbError> {
        if let Some(mrb) = self.mrb {
            Value::try_from_mrb(mrb, s.as_ref()).map_err(MrbError::Convert)
        } else {
            Err(MrbError::Closed)
        }
    }
}