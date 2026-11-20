//! The server's one HTTP boundary (ADR-0003): `rpc`, the HTTP/JSON adapter.
//!
//! `http` itself holds no code. The 2026-11-10 amendment to ADR-0003 names `http/rpc/` as the
//! one HTTP boundary — no `http/rest/` module exists — so this module is a declaration, and
//! every line of the adapter lives one directory down in [`rpc`].

pub(crate) mod rpc;

pub(crate) use rpc::serve;
