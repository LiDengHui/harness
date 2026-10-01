//! `#[harness::tool]` and `#[harness::skill]` attribute macros.
//!
//! A proc macro only ever sees the item it is attached to, and stable Rust
//! exposes no span-to-source-file API (it is behind the unstable
//! `proc_macro_span` feature), so an attribute on the handler function cannot
//! reach the argument struct that sits next to it. `#[tool]` therefore attaches
//! to the **argument struct** — the only place where the JSON Schema can be
//! derived from named fields at compile time — and resolves the handler by
//! name. Everything else the harness promises (the unit struct, the `Tool`
//! impl, the required list, the per-field descriptions) is generated as
//! specified.
//!
//! The generated code refers to `::harness_core`, `::harness_tools`,
//! `::serde_json` and `::async_trait` by absolute path, so a consumer needs
//! those crates as dependencies but no imports.

mod schema;
mod tool;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::parse::Parser;
use syn::{Item, LitStr};

/// The `name = "..."`-style arguments both macros accept.
#[derive(Default)]
struct Attrs {
    name: Option<LitStr>,
    description: Option<LitStr>,
    handler: Option<LitStr>,
    type_name: Option<LitStr>,
}

/// Parses `name`, `description` and — for `#[tool]` only — `handler` and
/// `type_name`.
fn parse_attrs(attr: TokenStream2, allow_handler: bool) -> syn::Result<Attrs> {
    let mut attrs = Attrs::default();

    let parser = syn::meta::parser(|meta| {
        let key = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        match key.as_str() {
            "name" => attrs.name = Some(meta.value()?.parse()?),
            "description" => attrs.description = Some(meta.value()?.parse()?),
            "handler" if allow_handler => attrs.handler = Some(meta.value()?.parse()?),
            "type_name" if allow_handler => attrs.type_name = Some(meta.value()?.parse()?),
            other => {
                let expected = if allow_handler {
                    "`name`, `description`, `handler` and `type_name`"
                } else {
                    "`name` and `description`"
                };
                let detail = if other.is_empty() {
                    format!("unsupported attribute argument; expected {expected}")
                } else {
                    format!("unsupported attribute argument `{other}`; expected {expected}")
                };
                return Err(meta.error(detail));
            }
        }
        Ok(())
    });
    parser.parse2(attr)?;

    Ok(attrs)
}

fn required(value: Option<LitStr>, macro_name: &str, key: &str) -> syn::Result<LitStr> {
    value.ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            format!("`#[harness::{macro_name}]` requires `{key} = \"...\"`"),
        )
    })
}

/// Turns an argument struct into a [`harness_tools::Tool`] implementation.
///
/// ```ignore
/// #[harness::tool(name = "read_file", description = "Read a file from the workspace")]
/// #[derive(serde::Deserialize)]
/// struct ReadArgs {
///     /// Path relative to the workspace root.
///     path: String,
///     /// 1-based first line to return.
///     offset: Option<u32>,
/// }
///
/// async fn read_file(args: ReadArgs, ctx: &harness_tools::ToolContext)
///     -> harness_core::Result<harness_tools::ToolOutput>
/// { /* body unchanged */ }
/// ```
///
/// The handler is the function named by `handler`, defaulting to `name` with
/// any character that cannot appear in an identifier replaced by `_`. The
/// generated unit struct is the PascalCase of the handler's last path segment
/// with `Tool` appended: `read_file` becomes `ReadFileTool`. A crate that
/// already names its tool type something else — or that wants a shorter name —
/// can override that with `type_name = "ReadFile"`.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let item = TokenStream2::from(item);

    match tool::expand(attr, item.clone()) {
        Ok(tokens) => tokens.into(),
        // Re-emitting the item keeps a bad attribute from cascading into a
        // "cannot find type" error at every use site.
        Err(err) => {
            let error = err.to_compile_error();
            quote!(#item #error).into()
        }
    }
}

/// Compiles a skill's name and description into the binary as a constant.
///
/// ```ignore
/// #[harness::skill(name = "system-design", description = "...")]
/// pub struct SystemDesign;
/// // => pub const SYSTEM_DESIGN_SKILL: (&str, &str) = ("system-design", "...");
/// ```
///
/// The pair is `(name, description)`, matching the two fields the registry
/// exposes before a skill is activated. The marker item itself is kept so the
/// attribute can be attached to whatever already names the skill.
#[proc_macro_attribute]
pub fn skill(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let item = TokenStream2::from(item);

    match expand_skill(attr, item.clone()) {
        Ok(tokens) => tokens.into(),
        Err(err) => {
            let error = err.to_compile_error();
            quote!(#item #error).into()
        }
    }
}

fn expand_skill(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let attrs = parse_attrs(attr, false)?;
    let name = required(attrs.name, "skill", "name")?;
    let description = required(attrs.description, "skill", "description")?;

    let item: Item = syn::parse2(item)?;
    let marker = marker_ident(&item).ok_or_else(|| {
        syn::Error::new_spanned(
            &item,
            "`#[harness::skill]` needs an item with a name to derive the constant from",
        )
    })?;
    let const_ident = syn::Ident::new(
        &format!("{}_SKILL", screaming_case(&marker.to_string())),
        marker.span(),
    );

    Ok(quote! {
        #item

        pub const #const_ident: (&str, &str) = (#name, #description);
    })
}

fn marker_ident(item: &Item) -> Option<&syn::Ident> {
    match item {
        Item::Struct(item) => Some(&item.ident),
        Item::Enum(item) => Some(&item.ident),
        Item::Fn(item) => Some(&item.sig.ident),
        Item::Const(item) => Some(&item.ident),
        Item::Static(item) => Some(&item.ident),
        Item::Mod(item) => Some(&item.ident),
        Item::Trait(item) => Some(&item.ident),
        _ => None,
    }
}

/// `SystemDesign` and `system_design` both become `SYSTEM_DESIGN`.
fn screaming_case(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch == '_' {
            out.push('_');
        } else if ch.is_uppercase() && !out.is_empty() && !out.ends_with('_') {
            out.push('_');
            out.extend(ch.to_uppercase());
        } else {
            out.extend(ch.to_uppercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screaming_case_handles_both_spellings() {
        assert_eq!(screaming_case("SystemDesign"), "SYSTEM_DESIGN");
        assert_eq!(screaming_case("system_design"), "SYSTEM_DESIGN");
        assert_eq!(screaming_case("ReadArgs"), "READ_ARGS");
        assert_eq!(screaming_case("a"), "A");
    }
}
