//! What a program file is.

use std::io::Read;

use super::*;

pub fn file_facts(_path: &str) -> Result<FileFacts, String> {
    Err("not available on Linux yet".into())
}

pub fn sha256_of(_data: impl Read) -> Option<String> {
    None
}

pub fn signature(_path: &str) -> Signature {
    Signature::Unsigned
}
