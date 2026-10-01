//! Expansion of `#[harness::tool]`.
//!
//! The attribute sits on the argument struct because that is the only item the
//! macro can both see and derive a schema from; see the crate docs.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{ExprPath, Fields, Item, ItemStruct, LitStr};

use crate::schema;
use crate::{parse_attrs, required};

pub(crate) fn expand(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let attrs = parse_attrs(attr, true)?;
    let name = required(attrs.name, "tool", "name")?;
    let description = required(attrs.description, "tool", "description")?;

    let args = argument_struct(item)?;
    let Fields::Named(named) = &args.fields else {
        return Err(syn::Error::new_spanned(
            &args,
            "the argument struct must have named fields: a tuple struct has no field names to describe",
        ));
    };

    let handler_source = match &attrs.handler {
        Some(handler) => handler.value(),
        None => default_handler(&name)?,
    };
    let handler: ExprPath = syn::parse_str(&handler_source).map_err(|_| {
        let span = attrs
            .handler
            .as_ref()
            .map_or_else(|| name.span(), syn::LitStr::span);
        syn::Error::new(
            span,
            format!("`{handler_source}` is not a valid handler path; name the handler function with `handler = \"...\"`"),
        )
    })?;

    let function = handler
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new(name.span(), "the handler path is empty"))?
        .ident
        .clone();
    let tool_ident = tool_ident(attrs.type_name.as_ref(), &function)?;

    let args_ident = &args.ident;
    let vis = &args.vis;
    let schema = schema::build(&named.named)?;

    Ok(quote! {
        #args

        #vis struct #tool_ident;

        #[::async_trait::async_trait]
        impl ::harness_tools::Tool for #tool_ident {
            fn spec(&self) -> ::harness_core::ToolSpec {
                ::harness_core::ToolSpec::new(#name, #description, #schema)
            }

            async fn call(
                &self,
                args: ::serde_json::Value,
                ctx: &::harness_tools::ToolContext,
            ) -> ::harness_core::Result<::harness_tools::ToolOutput> {
                let args: #args_ident = ::serde_json::from_value(args).map_err(|err| {
                    ::harness_core::HarnessError::Tool(format!(
                        "{}: invalid arguments: {err}",
                        #name
                    ))
                })?;
                #handler(args, ctx).await
            }
        }
    })
}

fn argument_struct(item: TokenStream2) -> syn::Result<ItemStruct> {
    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => Ok(item),
        Item::Fn(item) => Err(syn::Error::new_spanned(
            item.sig.ident,
            "`#[harness::tool]` belongs on the argument struct, not on the handler: a proc macro \
             cannot see a sibling item's fields, so the attribute has to be attached to the struct \
             the handler takes",
        )),
        other => Err(syn::Error::new_spanned(
            other,
            "`#[harness::tool]` can only be applied to the argument struct",
        )),
    }
}

/// `handler = "read_file"` names the generated struct `ReadFileTool`; an
/// explicit `type_name` replaces that, so a crate that already exposes a tool
/// type under a given name can adopt the macro without renaming it.
fn tool_ident(override_name: Option<&LitStr>, function: &syn::Ident) -> syn::Result<syn::Ident> {
    match override_name {
        Some(name) => syn::parse_str::<syn::Ident>(&name.value()).map_err(|_| {
            syn::Error::new(
                name.span(),
                format!(
                    "`{}` is not a valid name for the generated tool struct",
                    name.value()
                ),
            )
        }),
        None => Ok(syn::Ident::new(
            &format!("{}Tool", schema::pascal_case(&function.to_string())),
            function.span(),
        )),
    }
}

/// `name = "read_file"` implies a handler called `read_file`; a name that is not
/// identifier-shaped has to say so explicitly.
fn default_handler(name: &LitStr) -> syn::Result<String> {
    let handler: String = name
        .value()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();

    let usable = handler
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_');
    if !usable {
        return Err(syn::Error::new(
            name.span(),
            format!(
                "cannot derive a handler name from `{}`; set `handler = \"...\"`",
                name.value()
            ),
        ));
    }
    Ok(handler)
}
