//! Request handlers, one module per resource. Each finds the path's objects
//! first (404), then checks the body (400), then conflicts (409), and only
//! then changes anything (SPEC 3.4, 3.11).

pub mod accounts;
pub mod categories;
pub mod payees;
pub mod plans;
