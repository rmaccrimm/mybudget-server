//! Request handlers, one module per resource. Each takes the store's lock,
//! finds the path's objects first (404), then checks the body (400), then
//! conflicts (409), and only then changes anything (SPEC 3.4, 3.11).

pub mod accounts;
pub mod categories;
pub mod months;
pub mod payees;
pub mod plans;
pub mod transactions;
