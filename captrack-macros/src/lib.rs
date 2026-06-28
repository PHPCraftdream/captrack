//! `captrack-macros` — proc-macro companion for `captrack`.
//!
//! Provides `declare_collections!` which generates a family of thin delegating
//! macros that wrap `captrack`'s primitive macros with a custom default hasher
//! (Axis 2C).
//!
//! # Usage
//!
//! ```ignore
//! // In your crate (requires captrack in [dependencies]):
//! captrack::declare_collections! { hasher = MyHasher, prefix = my }
//!
//! // Generated macros:
//! //   my_vec!("name", cap)          → captrack::tvec!("name", cap)
//! //   my_vecdeque!("name", cap)     → captrack::tvecdeque!("name", cap)
//! //   my_btreemap!("name", cap)     → captrack::tbtreemap!("name", cap)
//! //   my_btreeset!("name", cap)     → captrack::tbtreeset!("name", cap)
//! //   my_bytesmut!("name", cap)     → captrack::tbytesmut!("name", cap)
//! //   my_fxmap!("name", cap)        → captrack::tfxmap!("name", cap; MyHasher::default())
//! //   my_fxset!("name", cap)        → captrack::tfxset!("name", cap; MyHasher::default())
//! //   my_map!("name", cap)          → captrack::tmap!("name", cap; MyHasher::default())
//! //   my_set!("name", cap)          → captrack::tset!("name", cap; MyHasher::default())
//! //   my_dashmap!("name", cap)      → captrack::tdashmap!("name", cap; MyHasher::default())
//! //   my_sccmap!("name", cap)       → captrack::tsccmap!("name", cap; MyHasher::default())
//! //   my_sccset!("name", cap)       → captrack::tsccset!("name", cap; MyHasher::default())
//! //   my_scctree!("name", cap)      → captrack::tscctree!("name", cap)
//!
//! let m = my_map!("my_module/rows", 64);
//! ```
//!
//! # Rationale — why proc-macro?
//!
//! Stable Rust `macro_rules!` cannot generate `macro_rules!` with dollar
//! metavariables (`$$` is not stable).  A proc-macro avoids that restriction
//! and emits thin delegation tokens that resolve telemetry on/off at captrack's
//! own feature level (not the consumer's).
//!
//! # Requirements
//!
//! The consuming crate must have `captrack` in its `[dependencies]` — the
//! generated macros call `::captrack::t*!` with absolute paths.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Path, Token};

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parsed form of `declare_collections! { hasher = MyHasher, prefix = my }`
struct DeclareArgs {
    hasher: Path,
    prefix: Ident,
}

impl Parse for DeclareArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // Parse: hasher = <path>, prefix = <ident>
        // Both fields are required; order is flexible.
        let mut hasher: Option<Path> = None;
        let mut prefix: Option<Ident> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let _eq: Token![=] = input.parse()?;

            match key.to_string().as_str() {
                "hasher" => {
                    hasher = Some(input.parse::<Path>()?);
                }
                "prefix" => {
                    prefix = Some(input.parse::<Ident>()?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown key `{other}`; expected `hasher` or `prefix`"),
                    ));
                }
            }

            // Optional trailing comma between fields.
            if input.peek(Token![,]) {
                let _: Token![,] = input.parse()?;
            }
        }

        Ok(DeclareArgs {
            hasher: hasher.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "missing required field `hasher`")
            })?,
            prefix: prefix.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "missing required field `prefix`")
            })?,
        })
    }
}

// ── Code generator ────────────────────────────────────────────────────────────

/// Generate a family of thin delegating macros that forward to `captrack`'s
/// primitives with the specified default hasher.
///
/// See the [module-level docs](self) for the full list of generated macros and
/// usage examples.
#[proc_macro]
pub fn declare_collections(input: TokenStream) -> TokenStream {
    let DeclareArgs { hasher, prefix } = match syn::parse::<DeclareArgs>(input) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    // Build per-macro ident names from the prefix.
    let mk = |suffix: &str| Ident::new(&format!("{}_{}", prefix, suffix), Span::call_site());

    let vec_name = mk("vec");
    let vecdeque_name = mk("vecdeque");
    let btreemap_name = mk("btreemap");
    let btreeset_name = mk("btreeset");
    let bytesmut_name = mk("bytesmut");
    let fxmap_name = mk("fxmap");
    let fxset_name = mk("fxset");
    let map_name = mk("map");
    let set_name = mk("set");
    let dashmap_name = mk("dashmap");
    let sccmap_name = mk("sccmap");
    let sccset_name = mk("sccset");
    let scctree_name = mk("scctree");

    let expanded = quote! {
        // ── Non-hash macros: no hasher needed — just delegate. ──────────────

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tvec!`.
        #[macro_export]
        macro_rules! #vec_name {
            ($n:literal, $c:expr) => { ::captrack::tvec!($n, $c) };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tvecdeque!`.
        #[macro_export]
        macro_rules! #vecdeque_name {
            ($n:literal, $c:expr) => { ::captrack::tvecdeque!($n, $c) };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tbtreemap!`.
        #[macro_export]
        macro_rules! #btreemap_name {
            ($n:literal, $c:expr) => { ::captrack::tbtreemap!($n, $c) };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tbtreeset!`.
        #[macro_export]
        macro_rules! #btreeset_name {
            ($n:literal, $c:expr) => { ::captrack::tbtreeset!($n, $c) };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tbytesmut!`.
        #[macro_export]
        macro_rules! #bytesmut_name {
            ($n:literal, $c:expr) => { ::captrack::tbytesmut!($n, $c) };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tscctree!`.
        #[macro_export]
        macro_rules! #scctree_name {
            ($n:literal, $c:expr) => { ::captrack::tscctree!($n, $c) };
        }

        // ── Hash macros: inject the custom hasher via the `;`-arm. ──────────

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tfxmap!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #fxmap_name {
            ($n:literal, $c:expr) => {
                ::captrack::tfxmap!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tfxmap!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tfxset!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #fxset_name {
            ($n:literal, $c:expr) => {
                ::captrack::tfxset!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tfxset!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tmap!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #map_name {
            ($n:literal, $c:expr) => {
                ::captrack::tmap!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tmap!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tset!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #set_name {
            ($n:literal, $c:expr) => {
                ::captrack::tset!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tset!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tdashmap!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #dashmap_name {
            ($n:literal, $c:expr) => {
                ::captrack::tdashmap!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tdashmap!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tsccmap!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #sccmap_name {
            ($n:literal, $c:expr) => {
                ::captrack::tsccmap!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tsccmap!($n, $c; $h)
            };
        }

        /// Generated by `captrack::declare_collections!` — delegates to `::captrack::tsccset!`
        /// with the custom default hasher.
        #[macro_export]
        macro_rules! #sccset_name {
            ($n:literal, $c:expr) => {
                ::captrack::tsccset!($n, $c; <#hasher as ::core::default::Default>::default())
            };
            ($n:literal, $c:expr; $h:expr) => {
                ::captrack::tsccset!($n, $c; $h)
            };
        }
    };

    expanded.into()
}
