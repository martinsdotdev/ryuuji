//! Where a token is kept between calls. The store is a module chosen for
//! the target, never a trait with one implementation: memory everywhere
//! today, and Windows' Credential Manager behind the same three items once
//! it lands.

mod memory;

pub(crate) use memory::Tokens;
