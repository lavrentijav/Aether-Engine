//! Regenerate `docs/BLOCK_PLACEMENT.md` and `docs/BLOCK_PLACEMENT.ru.md` from
//! the placement rule table.
//!
//!   cargo run -p aether-server --example gen_placement_md
//!
//! The same render is byte-compared against the checked-in files by
//! `placement::rules::tests::the_markdown_matches_the_checked_in_docs`, so the
//! docs cannot drift.

use aether_server::placement::rules::{self, Catalog, Lang};

fn main() {
    let cat = Catalog::builtin();
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for (file, lang) in [
        ("BLOCK_PLACEMENT.md", Lang::En),
        ("BLOCK_PLACEMENT.ru.md", Lang::Ru),
    ] {
        let path = docs.join(file);
        std::fs::write(&path, rules::render_markdown(lang, &cat))
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        println!("wrote {}", path.display());
    }
}
