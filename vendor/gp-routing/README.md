# Vendored gp-routing

This directory contains a source snapshot of
[RESMP-DEV/gp-routing](https://github.com/RESMP-DEV/gp-routing) at commit
`e471cba9816fe44b2d17a8136f124a8acb41e4b2`.

The upstream crate is licensed under Apache-2.0. CCR-Rust vendors it so public
downstream builds do not depend on a private Git repository.

CCR-Rust modifies the upstream feature encoder to support 32 backend slots and
a continuous relative request-cost feature. See `NOTICE` for the exact local
changes. The vendored crate remains a separate package and retains its upstream
name, authorship, repository metadata, and license.
