//! Pure mail logic: no I/O, no GTK, no threads.

pub mod account;
pub mod accounts_file;
pub mod actions;
pub mod attachment;
pub mod autoconfig;
pub mod bodies;
pub mod compose;
pub mod config;
pub mod contact;
pub mod flowed;
pub mod folders;
#[cfg(any(test, feature = "fuzz"))]
#[doc(hidden)]
pub mod fuzz;
pub mod html_depth;
pub mod html_scan;
pub mod keymap;
pub mod layout;
pub mod links;
pub mod mailto;
pub mod message;
pub mod model;
pub mod palette;
pub mod participants;
pub mod quotes;
pub mod reading;
pub mod remind;
pub mod search;
pub mod sieve;
pub mod snippet;
pub mod snippets;
pub mod split;
pub mod sync;
pub mod theme;
pub mod triage;
pub mod updates;
pub mod when;
