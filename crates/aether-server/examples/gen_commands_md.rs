//! Regenerate `docs/COMMANDS.md` and `docs/COMMANDS.ru.md` from the one
//! command catalogue (`aether_server::commands::catalogue`).
//!
//! Run from anywhere in the workspace:
//!
//! ```text
//! cargo-locked run -p aether-server --example gen_commands_md
//! ```
//!
//! The generated files are byte-compared to the catalogue by
//! `commands::catalogue::tests::the_generated_docs_are_in_sync`, so a change
//! to the list that is not followed by this command fails `cargo test`.

use std::path::Path;

use aether_server::commands::catalogue::{render_markdown, Lang};

fn main() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for (name, lang) in [("COMMANDS.md", Lang::En), ("COMMANDS.ru.md", Lang::Ru)] {
        let path = docs.join(name);
        std::fs::write(&path, render_markdown(lang))
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {}", path.display());
    }
}
