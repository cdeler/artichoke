use mruby::def::Parent;
use mruby::file::MrbFile;
use mruby::interpreter::Mrb;
use mruby::load::MrbLoadSources;
use mruby::MrbError;
use mruby_gems::Gem;
use std::borrow::Cow;
use std::convert::AsRef;
use std::rc::Rc;

pub fn init(interp: &Mrb) -> Result<(), MrbError> {
    Nemesis::init(interp)
}

#[derive(RustEmbed)]
// TODO: resolve path relative to CARGO_MANIFEST_DIR
// https://github.com/pyros2097/rust-embed/pull/59
#[folder = "nemesis/ruby/lib"]
struct Nemesis;

impl Nemesis {
    fn contents<T: AsRef<str>>(path: T) -> Result<Vec<u8>, MrbError> {
        let path = path.as_ref();
        Self::get(path)
            .map(Cow::into_owned)
            .ok_or_else(|| MrbError::SourceNotFound(path.to_owned()))
    }
}

impl MrbFile for Nemesis {
    fn require(interp: Mrb) -> Result<(), MrbError> {
        interp.borrow_mut().def_module::<Self>("Nemesis", None);
        Ok(())
    }
}

impl Gem for Nemesis {
    fn init(interp: &Mrb) -> Result<(), MrbError> {
        for source in Self::iter() {
            let contents = Self::contents(&source)?;
            interp.def_rb_source_file(source, contents)?;
        }
        interp.def_file_for_type::<_, Self>("nemesis.rb")?;
        interp.def_file_for_type::<_, Response>("nemesis/response.rb")?;
        Ok(())
    }
}

pub struct Response;

impl MrbFile for Response {
    fn require(interp: Mrb) -> Result<(), MrbError> {
        let parent = interp
            .borrow()
            .module_spec::<Nemesis>()
            .ok_or(MrbError::NotDefined("Nemesis".to_owned()))?;
        let parent = Parent::Module {
            spec: Rc::clone(&parent),
        };
        interp
            .borrow_mut()
            .def_class::<Self>("Response", Some(parent), None);
        Ok(())
    }
}
