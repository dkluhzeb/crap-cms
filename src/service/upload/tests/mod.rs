//! Tests of the upload write service: the file lifecycle across draft,
//! publish, prune and delete, and the settle step that releases files.

mod lifecycle;
mod publish;
mod support;
