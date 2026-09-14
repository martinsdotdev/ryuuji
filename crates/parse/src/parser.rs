mod validate;

use crate::element::Elements;
use crate::keyword::KeywordTable;

pub(crate) struct Parser<'a> {
    elements: &'a mut Elements,
    table: &'a KeywordTable,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(elements: &'a mut Elements, table: &'a KeywordTable) -> Parser<'a> {
        Parser { elements, table }
    }

    pub(crate) fn run(mut self) {
        self.validate_elements();
    }
}
