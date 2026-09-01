use super::{
    canonical_json_sha256, path_text, sha256, suffix_path, CarrierInput, CarrierKind, Failure,
    MaterializationInput, ServerInput, ToolInput,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const REGISTRY_SCHEMA: &str = "narada.site.capabilities.mcp_surfaces.v1";

#[derive(Debug)]
pub(crate) struct DeriveOptions {
    contract: PathBuf,
    workspace_root: PathBuf,
    home: PathBuf,
    matrix: PathBuf,
    installed_index: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    schema: String,
    sites: Vec<ContractSite>,
    carriers: Vec<ContractCarrier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractSite {
    site_id: String,
    registry_path: PathBuf,
    surface_ids: Vec<String>,
    #[serde(default)]
    admit_local_bindings: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractCarrier {
    carrier_id: String,
    carrier_kind: CarrierKind,
    config_relative_path: String,
    #[serde(default)]
    codex_plugin_overrides: BTreeMap<String, bool>,
}

impl DeriveOptions {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, Failure> {
        let mut values = BTreeMap::<String, PathBuf>::new();
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| {
                Failure::new("materializer_argument_invalid", "Argument is not UTF-8.")
            })?;
            if !matches!(
                flag.as_str(),
                "--contract" | "--workspace-root" | "--home" | "--matrix" | "--installed-index"
            ) {
                return Err(Failure::new(
                    "materializer_argument_unknown",
                    format!("Unknown argument: {flag}"),
                ));
            }
            let value = args.next().map(PathBuf::from).ok_or_else(|| {
                Failure::new(
                    "materializer_argument_value_required",
                    format!("{flag} requires a path."),
                )
            })?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(Failure::new(
                    "materializer_argument_duplicate",
                    format!("Duplicate argument: {flag}"),
                ));
            }
        }
        let take = |flag: &str| {
            values.get(flag).cloned().ok_or_else(|| {
                Failure::new("materializer_argument_required", format!("Missing {flag}."))
            })
        };
        Ok(Self {
            contract: take("--contract")?,
            workspace_root: take("--workspace-root")?,
            home: take("--home")?,
            matrix: take("--matrix")?,
            installed_index: take("--installed-index")?,
        })
    }
}

