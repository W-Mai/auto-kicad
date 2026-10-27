use kicad_adapter::FootprintRegistry;
use kicad_adapter::footprint::AllowlistedFootprint;
use kicad_adapter::library_catalog::{
    FootprintCatalog, LegacyLibraryMapping, LegacyMappingMode, LibraryCatalogPolicy,
};
use serde::{Deserialize, Deserializer};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) type SharedLibrarySettings = Arc<Result<LibrarySettings, String>>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryConfig {
    version: u32,
    global_table: Option<PathBuf>,
    #[serde(default)]
    extra_roots: Vec<PathBuf>,
    #[serde(default, deserialize_with = "unique_variables")]
    path_variables: BTreeMap<String, String>,
}

fn unique_variables<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique path variable keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate path variable"));
                }
            }
            Ok(values)
        }
    }
    d.deserialize_map(Visitor)
}

#[derive(Clone, Debug)]
pub(crate) struct LibrarySettings {
    config: LibraryConfig,
    config_digest: kicad_model::RevisionDigest,
    config_base: Option<PathBuf>,
    official_root: Option<PathBuf>,
    official_override: bool,
    project_override: Option<PathBuf>,
}

pub(crate) fn capture() -> SharedLibrarySettings {
    Arc::new(LibrarySettings::from_environment())
}

pub(crate) fn registry(
    settings: &SharedLibrarySettings,
    project: &Path,
    version: &str,
) -> Result<FootprintRegistry, String> {
    settings
        .as_ref()
        .as_ref()
        .map_err(Clone::clone)?
        .registry(project, version)
}

pub(crate) fn native_settings_paths(version: &str) -> Result<Vec<PathBuf>, String> {
    let settings = LibrarySettings::from_environment()?;
    let major: u16 = version
        .split('.')
        .next()
        .ok_or("missing KiCad version")?
        .parse()
        .map_err(|_| "invalid KiCad version")?;
    let base = settings
        .config_base
        .ok_or("native configuration directory unavailable")?
        .join(format!("{major}.0"));
    Ok(vec![
        base.join("kicad.json"),
        base.join("kicad_common.json"),
    ])
}

impl LibrarySettings {
    fn from_environment() -> Result<Self, String> {
        let (config, config_digest) = match std::env::var_os("AUTO_KICAD_LIBRARY_CONFIG") {
            Some(value) => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("AUTO_KICAD_LIBRARY_CONFIG must be absolute".into());
                }
                let before = fs::metadata(&path).map_err(|e| e.to_string())?;
                if !before.is_file() || before.len() > 64 * 1024 {
                    return Err("library configuration must be a bounded regular file".into());
                }
                let file = fs::File::open(&path).map_err(|e| e.to_string())?;
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                if !metadata.is_file() || metadata.len() > 64 * 1024 {
                    return Err("library configuration must be a bounded regular file".into());
                }
                let mut bytes = Vec::new();
                file.take(64 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                (parse_config(&bytes)?, crate::digest_bytes(&bytes))
            }
            None => (
                LibraryConfig {
                    version: 1,
                    global_table: None,
                    extra_roots: vec![],
                    path_variables: BTreeMap::new(),
                },
                crate::digest_bytes(b"default-library-config-v1"),
            ),
        };
        let official = std::env::var_os("AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT");
        let official_override = official.is_some();
        let official_root = official.map(PathBuf::from).or_else(|| {
            [
                "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints",
                "/usr/share/kicad/footprints",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_dir())
        });
        #[cfg(not(target_os = "windows"))]
        let home = std::env::var_os("HOME").map(PathBuf::from);
        #[cfg(target_os = "macos")]
        let config_base = home.map(|p| p.join("Library/Preferences/kicad"));
        #[cfg(target_os = "windows")]
        let config_base = std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("kicad"));
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let config_base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|p| p.join(".config")))
            .map(|p| p.join("kicad"));
        let config_base = std::env::var_os("KICAD_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or(config_base);
        if config_base.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err("KiCad configuration directory must be absolute".into());
        }
        Ok(Self {
            config,
            config_digest,
            config_base,
            official_root,
            official_override,
            project_override: std::env::var_os("AUTO_KICAD_PROJECT_FOOTPRINT_ROOT")
                .map(PathBuf::from),
        })
    }

    fn registry(&self, project: &Path, version: &str) -> Result<FootprintRegistry, String> {
        let major: u16 = version
            .split('.')
            .next()
            .ok_or("missing KiCad version")?
            .parse()
            .map_err(|_| "invalid KiCad major version")?;
        if major == 0 {
            return Err("invalid KiCad major version".into());
        }
        let global = self
            .config
            .global_table
            .clone()
            .or_else(|| {
                self.config_base
                    .as_ref()
                    .map(|p| p.join(format!("{major}.0/fp-lib-table")))
            })
            .ok_or("global library configuration location unavailable")?;
        let mut variables = self.config.path_variables.clone();
        if let Some(root) = &self.official_root {
            variables
                .entry(format!("KICAD{major}_FOOTPRINT_DIR"))
                .or_insert(root.to_str().ok_or("library root is not UTF-8")?.into());
        }
        let project = fs::canonicalize(project).map_err(|e| e.to_string())?;
        let mut legacy = Vec::new();
        if let Some(root) = &self.official_root {
            for library in [
                AllowlistedFootprint::OfficialLed1210,
                AllowlistedFootprint::OfficialResistor0603,
            ] {
                let identifier = library.identifier();
                legacy.push(LegacyLibraryMapping {
                    root: root.join(format!("{}.pretty", identifier.nickname)),
                    identifier,
                    mode: if self.official_override {
                        LegacyMappingMode::ExplicitOverride
                    } else {
                        LegacyMappingMode::MissingNicknameFallback
                    },
                });
            }
        }
        legacy.push(LegacyLibraryMapping {
            identifier: AllowlistedFootprint::ProjectTypeC.identifier(),
            root: self
                .project_override
                .clone()
                .unwrap_or_else(|| project.join("Type-C.pretty")),
            mode: if self.project_override.is_some() {
                LegacyMappingMode::ExplicitOverride
            } else {
                LegacyMappingMode::MissingNicknameFallback
            },
        });
        let mut policy = LibraryCatalogPolicy::new(
            &project,
            &global,
            self.official_root.as_deref(),
            &self.config.extra_roots,
            variables,
            legacy,
        )?
        .with_configuration_digest(self.config_digest.as_str())?;
        if let Some(base) = &self.config_base {
            policy = policy.with_native_settings(&base.join(format!("{major}.0/kicad.json")))?;
        }
        Ok(FootprintRegistry::from_catalog(FootprintCatalog::load(
            &policy,
        )?))
    }
}

fn parse_config(bytes: &[u8]) -> Result<LibraryConfig, String> {
    if bytes.len() > 64 * 1024 {
        return Err("library configuration exceeds size limit".into());
    }
    let config: LibraryConfig = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if config.version != 1
        || config.path_variables.contains_key("KIPRJMOD")
        || config
            .global_table
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        || config.extra_roots.len() > 64
        || config.path_variables.len() > 64
        || config.extra_roots.iter().any(|p| !p.is_absolute())
        || config
            .path_variables
            .values()
            .any(|v| !Path::new(v).is_absolute())
    {
        return Err("invalid library configuration version, bounds or paths".into());
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_rejects_duplicates_unknown_fields_and_unbounded_authority() {
        assert!(parse_config(br#"{"version":1,"path_variables":{"PARTS":"/tmp"}}"#).is_ok());
        for text in [
            r#"{"version":1,"version":1}"#,
            r#"{"version":2}"#,
            r#"{"version":1,"extra":true}"#,
            r#"{"version":1,"global_table":"relative"}"#,
            r#"{"version":1,"path_variables":{"PARTS":"/a","PARTS":"/b"}}"#,
            r#"{"version":1,"path_variables":{"KIPRJMOD":"/tmp"}}"#,
        ] {
            assert!(parse_config(text.as_bytes()).is_err(), "accepted {text}");
        }
        assert!(parse_config(&vec![b' '; 64 * 1024 + 1]).is_err());
    }
}
