// SPDX-FileCopyrightText: 2026 CASOON
// SPDX-License-Identifier: MIT

#![forbid(unsafe_code)]

mod diagnostics;
mod position;
mod server;

fn main() {
    if let Err(error) = server::run_stdio() {
        eprintln!("html-conform-lsp: {error}");
        std::process::exit(1);
    }
}
