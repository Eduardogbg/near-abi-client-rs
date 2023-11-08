use near_abi::{AbiFunctionKind, AbiParameters, AbiRoot, AbiType};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use schemafy_lib::{Expander, Generator, Schema};
use std::path::{Path, PathBuf};

pub fn generate_abi_client(
    near_abi: AbiRoot,
    contract_name: proc_macro2::Ident,
) -> proc_macro2::TokenStream {
    let schema_json = serde_json::to_string(&near_abi.body.root_schema).unwrap();

    let generator = Generator::builder().with_input_json(&schema_json).build();

    let (mut token_stream, schema) = generator.generate_with_schema();
    let mut expander = Expander::new(None, "", &schema);

    token_stream.extend(quote! {
      use near_sdk::serde::*;
      use near_sdk::*;

      pub struct #contract_name {
        pub contract: near_workspaces::Contract,
      }
    });

    let mut methods_stream = proc_macro2::TokenStream::new();

    for function in near_abi.body.functions {
        let name = format_ident!("{}", function.name);

        let (param_names, params): (Vec<Ident>, Vec<TokenStream>) = match function.params {
            AbiParameters::Borsh { args: _ } => panic!("Borsh is currently unsupported"),
            AbiParameters::Json { args } => args
                .iter()
                .map(|arg_param| (arg_param, format_ident!("{}", arg_param.name)))
                .map(|(arg_param, arg_name)| {
                    let arg_type = expand_subschema(&mut expander, &arg_param.type_schema);

                    (arg_name.clone(), quote! { #arg_name: #arg_type })
                })
                .unzip(),
        };

        let return_type = function
            .result
            .map(|r_type| match r_type {
                AbiType::Json { type_schema } => expand_subschema(&mut expander, &type_schema),
                AbiType::Borsh { type_schema: _ } => panic!("Borsh is currently unsupported"),
            })
            .unwrap_or_else(|| quote! { () });

        let name_str = name.to_string();

        let key_value_params: Vec<TokenStream> = param_names
            .iter()
            .map(|param_name| format!("\"{param_name}\": {param_name}").parse().unwrap())
            .collect();

        let args_json = if param_names.is_empty() {
            quote! { serde_json::json!({}) }
        } else {
            quote! { serde_json::json!({ #(#key_value_params),* }) }
        };

        match function.kind {
            AbiFunctionKind::Call => {
                // README: the patch is the deposit name to avoid collisions, revisit this later
                // PATCH:
                methods_stream.extend(quote! {
                  pub async fn #name(
                    &self,
                    gas: near_workspaces::types::Gas,
                    deposit_amount: near_workspaces::types::NearToken,
                    #(#params),*
                  ) -> near_workspaces::Result<#return_type> {
                    let result = self.contract
                      .call(#name_str)
                      .args_json(#args_json)
                      .gas(gas)
                      .deposit(deposit_amount)
                      .transact()
                      .await?;
                    Ok(result.json::<#return_type>()?)
                  }
                });
                // END PATCH
            }
            AbiFunctionKind::View => {
                methods_stream.extend(quote! {
                  pub async fn #name(
                    &self,
                    #(#params),*
                  ) -> near_workspaces::Result<#return_type> {
                    let result = self.contract
                      .call(#name_str)
                      .args_json(#args_json)
                      .view()
                      .await?;
                    Ok(result.json::<#return_type>()?)
                  }
                });
            }
        }
    }

    token_stream.extend(quote! {
      impl #contract_name {
        #methods_stream
      }
    });

    token_stream
}

pub fn read_abi(abi_path: impl AsRef<Path>) -> AbiRoot {
    let abi_path = if abi_path.as_ref().is_relative() {
        let crate_root = get_crate_root().unwrap();
        crate_root.join(&abi_path)
    } else {
        PathBuf::from(abi_path.as_ref())
    };

    let abi_json = std::fs::read_to_string(&abi_path)
        .unwrap_or_else(|err| panic!("Unable to read `{}`: {}", abi_path.to_string_lossy(), err));

    let deserializer = &mut serde_json::Deserializer::from_str(abi_json.as_str());
    let res: Result<AbiRoot, _> = serde_path_to_error::deserialize(deserializer);

    match res {
        Ok(root) => root,
        Err(err) => panic!("{},{}", err.path().to_string(), err.into_inner()),
    }
}

fn get_crate_root() -> std::io::Result<PathBuf> {
    if let Ok(path) = std::env::var("CARGO_MANIFEST_DIR") {
        return Ok(PathBuf::from(path));
    }

    let current_dir = std::env::current_dir()?;

    for p in current_dir.ancestors() {
        if std::fs::read_dir(p)?
            .into_iter()
            .filter_map(Result::ok)
            .any(|p| p.file_name().eq("Cargo.toml"))
        {
            return Ok(PathBuf::from(p));
        }
    }

    Ok(current_dir)
}

fn schemars_schema_to_schemafy(schema: &schemars::schema::Schema) -> Schema {
    let schema_json = serde_json::to_string(&schema).unwrap();
    serde_json::from_str(&schema_json).unwrap_or_else(|err| {
        panic!(
            "Could not convert schemars schema to schemafy model: {}",
            err
        )
    })
}

fn expand_subschema(
    expander: &mut Expander,
    schema: &schemars::schema::Schema,
) -> proc_macro2::TokenStream {
    let schemafy_schema = schemars_schema_to_schemafy(schema);

    // PATCH: this previously assumed identifiers, but generic types (e.g. Option) are not idents
    // format_ident!("{}", expander.expand_type_from_schema(&schemafy_schema).typ)
    let mut expanded_type = expander.expand_type_from_schema(&schemafy_schema).typ;
    if expanded_type == "Promise" {
        // TODO: figure out what to do about promises (keep them as JSON values?)
        // READ: https://github.com/near/near-abi-client-rs/issues/10
        expanded_type = "::serde_json::Value".to_string();
    } else if expanded_type.starts_with("Option") {
        // TODO: figure out how to make this hygienic (e.g. output ::std::option::Option instead of simply Option, without manually checking for it)
        expanded_type = "::std::option::".to_string() + &expanded_type
    }

    expanded_type.parse().unwrap()
}
