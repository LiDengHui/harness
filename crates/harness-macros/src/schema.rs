//! JSON Schema derivation from an argument struct's named fields.
//!
//! The mapping is deliberately small and closed: an argument type the model
//! cannot be told about is a compile error naming the field, never a silently
//! empty schema that would have the model guess.

use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, ToTokens};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, Field, GenericArgument, Lit, Meta, PathArguments, PathSegment, Token,
    Type,
};

/// The JSON Schema types an argument field may have.
enum Kind {
    String,
    Integer,
    Number,
    Boolean,
    Array(Box<Kind>),
}

/// Builds the argument schema for `spec()`.
pub(crate) fn build(fields: &Punctuated<Field, Token![,]>) -> syn::Result<TokenStream2> {
    let mut properties = Vec::new();
    let mut required = Vec::new();

    for field in fields {
        let Some(ident) = &field.ident else {
            return Err(syn::Error::new_spanned(
                field,
                "the argument struct must have named fields",
            ));
        };
        let name = ident.to_string();
        let (ty, optional) = match option_inner(&field.ty) {
            Some(inner) => (inner, true),
            None => (&field.ty, false),
        };
        let kind = kind_of(ty).ok_or_else(|| unsupported(field, &name))?;

        let key = syn::LitStr::new(&name, ident.span());
        let value = property_tokens(&kind, doc_comment(&field.attrs).as_deref());
        properties.push(quote!(#key: #value));
        if !optional {
            required.push(key);
        }
    }

    Ok(quote! {
        ::harness_core::object_schema(
            ::serde_json::json!({ #(#properties),* }),
            &[ #(#required),* ],
        )
    })
}

/// `read_file` becomes `ReadFile`.
pub(crate) fn pascal_case(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn property_tokens(kind: &Kind, description: Option<&str>) -> TokenStream2 {
    let describe = match description {
        Some(text) => quote!(, "description": #text),
        None => TokenStream2::new(),
    };

    match kind {
        Kind::String => quote!({ "type": "string" #describe }),
        Kind::Integer => quote!({ "type": "integer" #describe }),
        Kind::Number => quote!({ "type": "number" #describe }),
        Kind::Boolean => quote!({ "type": "boolean" #describe }),
        Kind::Array(items) => {
            // The field's description covers the array; there is no separate
            // place to describe the elements in the schema dialect the builtin
            // tools use.
            let items = property_tokens(items, None);
            quote!({ "type": "array", "items": #items #describe })
        }
    }
}

/// The `T` of an `Option<T>`, which every caller treats as "not required".
fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    first_type_argument(segment)
}

fn first_type_argument(segment: &PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn kind_of(ty: &Type) -> Option<Kind> {
    match ty {
        // `&str` and `&Path` are as common in a signature as their owned forms.
        Type::Reference(reference) => kind_of(&reference.elem),
        Type::Path(path) => {
            let segment = path.path.segments.last()?;
            match segment.ident.to_string().as_str() {
                "String" | "str" | "PathBuf" | "Path" => Some(Kind::String),
                "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
                    Some(Kind::Integer)
                }
                "f32" | "f64" => Some(Kind::Number),
                "bool" => Some(Kind::Boolean),
                "Vec" => Some(Kind::Array(Box::new(kind_of(first_type_argument(
                    segment,
                )?)?))),
                _ => None,
            }
        }
        _ => None,
    }
}

fn unsupported(field: &Field, name: &str) -> syn::Error {
    syn::Error::new_spanned(
        &field.ty,
        format!(
            "argument field `{name}` has the unsupported type `{}`; supported types are String, \
             &str, PathBuf, Path, i8..i64, u8..u64, usize, isize, f32/f64, bool, Vec<T> and \
             Option<T>",
            field.ty.to_token_stream()
        ),
    )
}

/// Doc comment lines become the schema's `description`.
fn doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(meta) = &attr.meta {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) = &meta.value
            {
                let line = text.value();
                let line = line.trim();
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
        }
    }

    (!lines.is_empty()).then(|| lines.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_splits_on_underscores() {
        assert_eq!(pascal_case("read_file"), "ReadFile");
        assert_eq!(pascal_case("grep"), "Grep");
        assert_eq!(pascal_case("list_dir"), "ListDir");
        assert_eq!(pascal_case("a__b"), "AB");
    }

    #[test]
    fn option_inner_only_matches_option() {
        let option: Type = syn::parse_str("Option<u32>").unwrap();
        assert_eq!(quote!(#option).to_string(), "Option < u32 >");
        assert!(option_inner(&option).is_some());

        let plain: Type = syn::parse_str("u32").unwrap();
        assert!(option_inner(&plain).is_none());

        let nested: Type = syn::parse_str("Vec<Option<u32>>").unwrap();
        assert!(option_inner(&nested).is_none());
    }

    #[test]
    fn kinds_cover_the_documented_types() {
        for src in ["String", "&str", "PathBuf", "std::path::Path"] {
            let ty: Type = syn::parse_str(src).unwrap();
            assert!(matches!(kind_of(&ty), Some(Kind::String)), "{src}");
        }
        for src in ["u32", "i64", "usize", "isize"] {
            let ty: Type = syn::parse_str(src).unwrap();
            assert!(matches!(kind_of(&ty), Some(Kind::Integer)), "{src}");
        }
        for src in ["f32", "f64"] {
            let ty: Type = syn::parse_str(src).unwrap();
            assert!(matches!(kind_of(&ty), Some(Kind::Number)), "{src}");
        }
        let ty: Type = syn::parse_str("bool").unwrap();
        assert!(matches!(kind_of(&ty), Some(Kind::Boolean)));

        let ty: Type = syn::parse_str("Vec<String>").unwrap();
        assert!(matches!(kind_of(&ty), Some(Kind::Array(_))));

        for src in ["std::collections::HashMap<String, String>", "()", "u128"] {
            let ty: Type = syn::parse_str(src).unwrap();
            assert!(
                kind_of(&ty).is_none(),
                "{src} must not silently become a schema"
            );
        }
    }
}
