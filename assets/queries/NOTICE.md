# Highlight queries

These tree-sitter query files are copied, unchanged, from
[gpui-component](https://github.com/longbridge/gpui-component) 0.5.1
(`src/highlighter/languages/`), which is licensed under the Apache License 2.0 —
see [`LICENSE-APACHE`](LICENSE-APACHE) in this folder.

They are here because PulseTerm turns off gpui-component's
`tree-sitter-languages` feature (it bundles every grammar, ~30 MB of the release
binary) and registers the languages it keeps itself, in `src/ui/languages.rs`.
For those languages gpui-component used these files rather than the grammar
crates' own queries, so they are kept to leave highlighting exactly as it was.
Languages not listed here use the query that ships with their grammar crate.
