//! AST scan: parse .rs files with syn, find collection-constructor call-sites,
//! emit `AllocSite` records with exact byte ranges for in-place patching.
//!
//! The byte ranges are what the patcher (Step 11) splices, so getting them
//! right is the linchpin of the whole tool — a one-byte offset error
//! corrupts the file.  See module tests for the contract.

#![allow(dead_code)] // lifts when plan.rs (Step 9) starts using these

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, Item};

use crate::model::{AllocSite, CapExpr, Ctor, SiteKey};

/// Scan one `.rs` file and return all allocation sites found in it.
pub fn scan_file(path: &Path, include_tests: bool) -> Result<Vec<AllocSite>> {
    let src = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    scan_str(&src, path.to_path_buf(), include_tests)
}

/// Scan a string of Rust source.  Exposed for tests and to keep the file-IO
/// concern out of the visitor logic.
pub fn scan_str(src: &str, file: PathBuf, include_tests: bool) -> Result<Vec<AllocSite>> {
    let parsed: syn::File =
        syn::parse_file(src).with_context(|| format!("parse {}", file.display()))?;
    let line_offsets = build_line_offsets(src);

    let mut collector = SiteCollector {
        sites: Vec::new(),
        src,
        line_offsets,
        file,
        include_tests,
        in_test_module: false,
    };
    collector.visit_file(&parsed);
    Ok(collector.sites)
}

/// Build a vector mapping line numbers (0-indexed) to byte offsets of the
/// start of that line.  Line 1 (1-indexed) → index 0, etc.
///
/// Note: on Windows files may have CRLF line endings.  `proc_macro2` spans
/// report column as a byte offset within the logical line — the `\r` is part
/// of the preceding line's content, so columns in the *next* line start after
/// `\r\n`.  Because we count `\n` and push `i + 1` (the byte *after* `\n`),
/// the `\r` ends up as part of the *content bytes* of the line, which matches
/// what proc_macro2 reports for the column of the first non-whitespace char.
/// In practice all fixture files and real Rust sources in this repo are LF,
/// so CRLF is handled defensively but not explicitly tested.
fn build_line_offsets(src: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

fn span_to_byte_range(line_offsets: &[usize], span: Span) -> Option<Range<usize>> {
    let start = span.start();
    let end = span.end();
    // proc_macro2 uses 1-indexed lines; line 0 is invalid.
    if start.line == 0 || end.line == 0 {
        return None;
    }
    let s = line_offsets
        .get(start.line - 1)?
        .checked_add(start.column)?;
    let e = line_offsets.get(end.line - 1)?.checked_add(end.column)?;
    if e >= s {
        Some(s..e)
    } else {
        None
    }
}

fn site_key(file: &Path, span: Span) -> SiteKey {
    let lc = span.start();
    SiteKey {
        file: file.to_path_buf(),
        line: lc.line as u32,
        col: lc.column as u32,
    }
}

struct SiteCollector<'src> {
    sites: Vec<AllocSite>,
    src: &'src str,
    line_offsets: Vec<usize>,
    file: PathBuf,
    include_tests: bool,
    in_test_module: bool,
}

impl<'src> SiteCollector<'src> {
    fn skip(&self) -> bool {
        !self.include_tests && self.in_test_module
    }

    /// Extract the source text for a span; used for Dynamic-expr capture.
    fn span_text(&self, span: Span) -> Option<String> {
        let r = span_to_byte_range(&self.line_offsets, span)?;
        self.src.get(r).map(str::to_string)
    }

    fn arg_to_cap_expr(&self, expr: &Expr) -> CapExpr {
        if let Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(li),
            ..
        }) = expr
        {
            if let Ok(v) = li.base10_parse::<usize>() {
                return CapExpr::Literal(v);
            }
        }
        let text = self.span_text(expr.span()).unwrap_or_default();
        CapExpr::Dynamic(text)
    }

    /// Try to record an `AllocSite` for an `ExprCall` whose `func` is a path
    /// ending in `Vec::with_capacity`, `HashMap::new`, etc.
    fn try_record_call(&mut self, call: &syn::ExprCall) {
        let func_path = match call.func.as_ref() {
            Expr::Path(ep) => &ep.path,
            _ => return,
        };

        let (ctor, leaf) = match classify_path(func_path) {
            Some(pair) => pair,
            None => return,
        };

        let span = call.span();
        let key = site_key(&self.file, span);

        match leaf.as_str() {
            "new" => {
                // Vec::new() — span_bytes covers the entire call expression.
                let span_bytes = match span_to_byte_range(&self.line_offsets, span) {
                    Some(r) => r,
                    None => return,
                };
                self.sites.push(AllocSite {
                    key,
                    ctor,
                    current_cap: CapExpr::Zero,
                    span_bytes,
                });
            }
            "with_capacity" => {
                // with_capacity(cap) — span_bytes is the first argument.
                let cap_arg = match call.args.first() {
                    Some(a) => a,
                    None => return,
                };
                let span_bytes = match span_to_byte_range(&self.line_offsets, cap_arg.span()) {
                    Some(r) => r,
                    None => return,
                };
                let current_cap = self.arg_to_cap_expr(cap_arg);
                self.sites.push(AllocSite {
                    key,
                    ctor,
                    current_cap,
                    span_bytes,
                });
            }
            "with_capacity_and_hasher" => {
                // with_capacity_and_hasher(cap, hasher) — span_bytes is the
                // cap argument only; the patcher replaces just that token.
                let cap_arg = match call.args.first() {
                    Some(a) => a,
                    None => return,
                };
                let span_bytes = match span_to_byte_range(&self.line_offsets, cap_arg.span()) {
                    Some(r) => r,
                    None => return,
                };
                let current_cap = self.arg_to_cap_expr(cap_arg);
                self.sites.push(AllocSite {
                    key,
                    ctor,
                    current_cap,
                    span_bytes,
                });
            }
            _ => {}
        }
    }
}

/// Classify a `syn::Path` as a known collection constructor.
///
/// Returns `(Ctor, method_name)` when the second-to-last segment is a
/// recognised collection type and the last segment is one of `new`,
/// `with_capacity`, or `with_capacity_and_hasher`.  Returns `None` otherwise.
fn classify_path(path: &syn::Path) -> Option<(Ctor, String)> {
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    if segs.len() < 2 {
        return None;
    }
    let kind = match segs[segs.len() - 2].as_str() {
        "Vec" => Ctor::Vec,
        "VecDeque" => Ctor::VecDeque,
        "HashMap" => Ctor::HashMap,
        "HashSet" => Ctor::HashSet,
        "BTreeMap" => Ctor::BTreeMap,
        "BTreeSet" => Ctor::BTreeSet,
        _ => return None,
    };
    let leaf = segs.last().unwrap().clone();
    if !matches!(
        leaf.as_str(),
        "with_capacity" | "with_capacity_and_hasher" | "new"
    ) {
        return None;
    }
    Some((kind, leaf))
}

/// Returns `true` when the attribute list contains `#[cfg(test)]`.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        // Match `#[cfg(test)]`
        if !attr.path().is_ident("cfg") {
            return false;
        }
        // Parse the argument list as a Meta; look for `test` ident.
        if let syn::Meta::List(ml) = &attr.meta {
            // tokens should be just `test`
            let tokens_str = ml.tokens.to_string();
            return tokens_str.trim() == "test";
        }
        false
    })
}

impl<'src, 'ast> Visit<'ast> for SiteCollector<'src> {
    // -----------------------------------------------------------------------
    // Module / item boundary — handle #[cfg(test)] modules and macro_rules!
    // -----------------------------------------------------------------------
    fn visit_item(&mut self, item: &'ast Item) {
        match item {
            Item::Mod(item_mod) => {
                let is_test_mod = has_cfg_test(&item_mod.attrs);
                let was = self.in_test_module;
                if is_test_mod {
                    self.in_test_module = true;
                }
                // Use default traversal so we recurse into the mod body.
                syn::visit::visit_item_mod(self, item_mod);
                self.in_test_module = was;
            }
            Item::Macro(item_mac) => {
                // `macro_rules! foo { ... }` — do NOT descend into the body.
                // syn represents the body as opaque token-stream, and any
                // Rust-like constructs inside are pattern templates, not real
                // call expressions.  Visiting them would produce false positives.
                let _ = item_mac; // intentionally skipped
            }
            _ => {
                // All other items — default recursion.
                syn::visit::visit_item(self, item);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Expression level — catch ExprCall
    // -----------------------------------------------------------------------
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if !self.skip() {
            self.try_record_call(call);
        }
        // Always recurse into sub-expressions (arguments may contain nested
        // calls that are also allocation sites).
        syn::visit::visit_expr_call(self, call);
    }

    // -----------------------------------------------------------------------
    // Macro invocations — catch `vec![]`
    // -----------------------------------------------------------------------
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if self.skip() {
            syn::visit::visit_macro(self, mac);
            return;
        }

        // We only care about `vec!` with *empty* token stream — that is the
        // "zero-capacity allocate" case.  `vec![1, 2, 3]` is not a capacity
        // hint; `vec![0; n]` is initialisation with fill — skip both.
        let is_vec_macro = mac.path.is_ident("vec")
            || mac
                .path
                .segments
                .iter()
                .last()
                .map(|s| s.ident == "vec")
                .unwrap_or(false);

        if is_vec_macro && mac.tokens.is_empty() {
            let span = mac.span();
            let key = site_key(&self.file, span);
            if let Some(span_bytes) = span_to_byte_range(&self.line_offsets, span) {
                self.sites.push(AllocSite {
                    key,
                    ctor: Ctor::Vec,
                    current_cap: CapExpr::Zero,
                    span_bytes,
                });
            }
        }

        syn::visit::visit_macro(self, mac);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: scan a source string with a dummy path.
    fn scan(src: &str) -> Vec<AllocSite> {
        scan_str(src, PathBuf::from("test.rs"), false).expect("scan_str failed")
    }

    fn scan_with_tests(src: &str) -> Vec<AllocSite> {
        scan_str(src, PathBuf::from("test.rs"), true).expect("scan_str failed")
    }

    // -----------------------------------------------------------------------
    // Fixture-based tests (integration-style, using files on disk)
    // -----------------------------------------------------------------------

    fn fixture_path(name: &str) -> PathBuf {
        // tests/fixtures/ relative to the crate root (CARGO_MANIFEST_DIR).
        let manifest = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set — run via cargo test");
        PathBuf::from(manifest)
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn fixture_vec_with_capacity_one_site() {
        let sites = scan_file(&fixture_path("scan_vec_with_capacity.rs"), false).unwrap();
        assert_eq!(sites.len(), 1, "expected exactly 1 site, got {sites:?}");
        let s = &sites[0];
        assert_eq!(s.ctor, Ctor::Vec);
        assert_eq!(s.current_cap, CapExpr::Literal(16));
        // Verify the span points at the literal `16` in the source.
        let src = std::fs::read_to_string(fixture_path("scan_vec_with_capacity.rs")).unwrap();
        let text = &src[s.span_bytes.clone()];
        assert_eq!(
            text, "16",
            "span_bytes should cover the literal `16`, got {text:?}"
        );
    }

    #[test]
    fn fixture_vec_new_and_macro_two_sites() {
        let sites = scan_file(&fixture_path("scan_vec_new.rs"), false).unwrap();
        assert_eq!(sites.len(), 2, "expected 2 sites, got {sites:?}");
        for s in &sites {
            assert_eq!(s.ctor, Ctor::Vec);
            assert_eq!(s.current_cap, CapExpr::Zero);
        }
        // Span for Vec::new() should cover the full call.
        let src = std::fs::read_to_string(fixture_path("scan_vec_new.rs")).unwrap();
        let new_site = sites.iter().find(|s| {
            src.get(s.span_bytes.clone())
                .map(|t| t.starts_with("Vec::new"))
                .unwrap_or(false)
        });
        assert!(
            new_site.is_some(),
            "should have a site with span starting at Vec::new"
        );

        let vec_macro_site = sites.iter().find(|s| {
            src.get(s.span_bytes.clone())
                .map(|t| t.starts_with("vec!"))
                .unwrap_or(false)
        });
        assert!(
            vec_macro_site.is_some(),
            "should have a site with span starting at vec![]"
        );
    }

    #[test]
    fn fixture_with_capacity_and_hasher_arg_span() {
        let sites = scan_file(&fixture_path("scan_hashmap_with_hasher.rs"), false).unwrap();
        assert_eq!(sites.len(), 1, "expected 1 site, got {sites:?}");
        let s = &sites[0];
        assert_eq!(s.ctor, Ctor::HashMap);
        assert_eq!(s.current_cap, CapExpr::Literal(32));
        // Span must be on `32`, not the whole call.
        let src = std::fs::read_to_string(fixture_path("scan_hashmap_with_hasher.rs")).unwrap();
        let text = &src[s.span_bytes.clone()];
        assert_eq!(text, "32", "span_bytes should cover `32`, got {text:?}");
    }

    #[test]
    fn fixture_dynamic_cap() {
        let sites = scan_file(&fixture_path("scan_dynamic_cap.rs"), false).unwrap();
        assert_eq!(sites.len(), 1, "expected 1 site, got {sites:?}");
        let s = &sites[0];
        assert_eq!(s.ctor, Ctor::Vec);
        assert!(
            matches!(&s.current_cap, CapExpr::Dynamic(t) if t == "n * 2"),
            "expected Dynamic(\"n * 2\"), got {:?}",
            s.current_cap
        );
    }

    #[test]
    fn fixture_skips_macro_rules_body() {
        let sites = scan_file(&fixture_path("scan_skip_macro_body.rs"), false).unwrap();
        assert_eq!(
            sites.len(),
            0,
            "macro_rules! body must not yield any sites; got {sites:?}"
        );
    }

    #[test]
    fn fixture_skips_test_module_by_default() {
        let sites = scan_file(&fixture_path("scan_test_module_skipped.rs"), false).unwrap();
        assert_eq!(
            sites.len(),
            1,
            "default (include_tests=false) should yield only the main site; got {sites:?}"
        );
        assert_eq!(sites[0].current_cap, CapExpr::Literal(8));
    }

    #[test]
    fn fixture_include_tests_yields_both_sites() {
        let sites = scan_file(&fixture_path("scan_test_module_skipped.rs"), true).unwrap();
        assert_eq!(
            sites.len(),
            2,
            "include_tests=true should yield 2 sites; got {sites:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Inline unit tests — fast, no disk I/O
    // -----------------------------------------------------------------------

    #[test]
    fn inline_vec_with_capacity_literal() {
        let src = "fn f() { let _v: Vec<u8> = Vec::with_capacity(42); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::Vec);
        assert_eq!(sites[0].current_cap, CapExpr::Literal(42));
        assert_eq!(&src[sites[0].span_bytes.clone()], "42");
    }

    #[test]
    fn inline_vec_new_span_covers_full_call() {
        let src = "fn f() { let _v: Vec<u8> = Vec::new(); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::Vec);
        assert_eq!(sites[0].current_cap, CapExpr::Zero);
        let text = &src[sites[0].span_bytes.clone()];
        assert_eq!(text, "Vec::new()", "span must cover the full call");
    }

    #[test]
    fn inline_vec_macro_empty() {
        let src = "fn f() { let _v: Vec<u8> = vec![]; }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::Vec);
        assert_eq!(sites[0].current_cap, CapExpr::Zero);
        let text = &src[sites[0].span_bytes.clone()];
        assert!(
            text.starts_with("vec!"),
            "span must start with vec!, got {text:?}"
        );
    }

    #[test]
    fn inline_vec_macro_non_empty_skipped() {
        // vec![1, 2, 3] has content — not a zero-capacity allocation.
        let src = "fn f() { let _v = vec![1u8, 2, 3]; }";
        let sites = scan(src);
        assert_eq!(sites.len(), 0, "non-empty vec! should be skipped");
    }

    #[test]
    fn inline_vecdeque_with_capacity() {
        let src = "use std::collections::VecDeque; fn f() { let _d = VecDeque::with_capacity(8); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::VecDeque);
        assert_eq!(sites[0].current_cap, CapExpr::Literal(8));
        assert_eq!(&src[sites[0].span_bytes.clone()], "8");
    }

    #[test]
    fn inline_hashmap_new() {
        let src =
            "use std::collections::HashMap; fn f() { let _m: HashMap<u8,u8> = HashMap::new(); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::HashMap);
        assert_eq!(sites[0].current_cap, CapExpr::Zero);
    }

    #[test]
    fn inline_hashset_with_capacity() {
        let src =
            "use std::collections::HashSet; fn f() { let _s = HashSet::<u8>::with_capacity(4); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::HashSet);
        assert_eq!(sites[0].current_cap, CapExpr::Literal(4));
        assert_eq!(&src[sites[0].span_bytes.clone()], "4");
    }

    #[test]
    fn inline_btreemap_new() {
        let src =
            "use std::collections::BTreeMap; fn f() { let _b: BTreeMap<u8,u8> = BTreeMap::new(); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::BTreeMap);
        assert_eq!(sites[0].current_cap, CapExpr::Zero);
    }

    #[test]
    fn inline_btreeset_new() {
        let src =
            "use std::collections::BTreeSet; fn f() { let _b: BTreeSet<u8> = BTreeSet::new(); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::BTreeSet);
        assert_eq!(sites[0].current_cap, CapExpr::Zero);
    }

    #[test]
    fn inline_dynamic_cap_expression() {
        let src = "fn f(n: usize) { let _v: Vec<u8> = Vec::with_capacity(n * 2); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert!(
            matches!(&sites[0].current_cap, CapExpr::Dynamic(t) if t == "n * 2"),
            "got {:?}",
            sites[0].current_cap
        );
    }

    #[test]
    fn inline_method_call_not_captured() {
        // `foo.with_capacity(8)` must NOT be captured — it's a method call on
        // an existing value, not a Ctor::* call.
        let src = "fn f(foo: Vec<u8>) { foo.with_capacity(8); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 0, "method call on instance must be ignored");
    }

    #[test]
    fn inline_test_module_skipped() {
        let src = r#"
fn main() { let _v: Vec<u8> = Vec::with_capacity(1); }
#[cfg(test)]
mod tests {
    fn t() { let _x: Vec<u8> = Vec::with_capacity(99); }
}
"#;
        let sites = scan(src);
        assert_eq!(sites.len(), 1, "test module should be skipped by default");
        assert_eq!(sites[0].current_cap, CapExpr::Literal(1));
    }

    #[test]
    fn inline_test_module_included_when_flag_set() {
        let src = r#"
fn main() { let _v: Vec<u8> = Vec::with_capacity(1); }
#[cfg(test)]
mod tests {
    fn t() { let _x: Vec<u8> = Vec::with_capacity(99); }
}
"#;
        let sites = scan_with_tests(src);
        assert_eq!(sites.len(), 2, "include_tests=true should see both sites");
    }

    #[test]
    fn inline_macro_rules_body_skipped() {
        let src = r#"
macro_rules! make_vec { () => { Vec::with_capacity(99) }; }
fn main() { let _ = make_vec!(); }
"#;
        let sites = scan(src);
        assert_eq!(sites.len(), 0, "macro_rules body must not yield sites");
    }

    #[test]
    fn inline_with_capacity_and_hasher_arg_span() {
        let src = "use std::collections::HashMap;\nfn f() { let _m = HashMap::with_capacity_and_hasher(32, Default::default()); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ctor, Ctor::HashMap);
        assert_eq!(sites[0].current_cap, CapExpr::Literal(32));
        let text = &src[sites[0].span_bytes.clone()];
        assert_eq!(text, "32", "span_bytes must cover only `32`, got {text:?}");
    }

    #[test]
    fn inline_nested_calls_both_captured() {
        // Both the outer and inner allocation sites should be found.
        let src = r#"
fn f() {
    let _a: Vec<u8> = Vec::with_capacity(10);
    let _b: Vec<u8> = Vec::with_capacity(20);
}
"#;
        let sites = scan(src);
        assert_eq!(sites.len(), 2);
        let caps: Vec<_> = sites.iter().map(|s| &s.current_cap).collect();
        assert!(caps.contains(&&CapExpr::Literal(10)));
        assert!(caps.contains(&&CapExpr::Literal(20)));
    }

    #[test]
    fn inline_span_byte_range_is_tight() {
        // Sanity: the byte range must not extend past the end of the source.
        let src = "fn f() { let _v: Vec<u8> = Vec::with_capacity(7); }";
        let sites = scan(src);
        assert_eq!(sites.len(), 1);
        assert!(
            sites[0].span_bytes.end <= src.len(),
            "span end {} exceeds source length {}",
            sites[0].span_bytes.end,
            src.len()
        );
        assert_eq!(&src[sites[0].span_bytes.clone()], "7");
    }
}
