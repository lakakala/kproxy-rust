use std::fmt;
use std::error;


#[derive(Debug)]
pub struct ParseError {}

impl ParseError {
    pub fn new() -> ParseError {
        ParseError {}
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}
impl error::Error for ParseError {}