use super::{LoadError, Records};
use crate::session::Session;

pub fn parse(_records: &Records, _session: &mut Session) -> Result<(), LoadError> {
    Err(LoadError::Parse("codex adapter not implemented".into()))
}
