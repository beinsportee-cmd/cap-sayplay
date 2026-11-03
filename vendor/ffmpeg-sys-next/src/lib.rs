#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::approx_constant)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::redundant_static_lifetimes)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// bindings.rs is a single multi-megabyte line, so each suggestion from these
// lints echoes the whole file into the build log and buries real errors.
#![allow(unknown_lints)]
#![allow(unnecessary_transmutes)]
#![allow(unpredictable_function_pointer_comparisons)]

extern crate libc;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[macro_use]
mod avutil;
pub use avutil::*;
