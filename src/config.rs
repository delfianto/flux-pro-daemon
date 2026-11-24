use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use serde::{Deserialize, Deserializer, de};
use thiserror::Error;

pub const DEFAULT_PATH: &str = "/etc/fluxprod.toml";
pub const CONFIG_EXIT_CODE: u8 = 78;
const DEFAULT_INTERVAL_MS: u64 = 1_000;
const INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 250..=60_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    update_interval_ms: u64,
    pub cpu: Source,
    pub gpu: Source,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            update_interval_ms: DEFAULT_INTERVAL_MS,
            cpu: Source::Auto,
            gpu: Source::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "SourceTable")]
pub enum Source {
    Auto,
    Off,
    Hwmon {
        chip: String,
        sensor: String,
        pci_address: Option<PciAddress>,
    },
    Nvml {
        index: Option<u32>,
        pci_address: Option<PciAddress>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceKind {
    Auto,
    Off,
    Hwmon,
    Nvml,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceTable {
    source: SourceKind,
    chip: Option<String>,
    sensor: Option<String>,
    index: Option<u32>,
    pci_address: Option<PciAddress>,
}

impl TryFrom<SourceTable> for Source {
    type Error = &'static str;

    fn try_from(table: SourceTable) -> Result<Self, Self::Error> {
        let SourceTable {
            source,
            chip,
            sensor,
            index,
            pci_address,
        } = table;
        match source {
            SourceKind::Auto | SourceKind::Off
                if chip.is_some()
                    || sensor.is_some()
                    || index.is_some()
                    || pci_address.is_some() =>
            {
                Err("sources auto and off accept only the source key")
            }
            SourceKind::Auto => Ok(Self::Auto),
            SourceKind::Off => Ok(Self::Off),
            SourceKind::Hwmon if index.is_some() => Err("source hwmon does not accept index"),
            SourceKind::Hwmon => Ok(Self::Hwmon {
                chip: chip.ok_or("source hwmon requires chip")?,
                sensor: sensor.ok_or("source hwmon requires sensor")?,
                pci_address,
            }),
            SourceKind::Nvml if chip.is_some() || sensor.is_some() => {
                Err("source nvml accepts only index or pci_address")
            }
            SourceKind::Nvml => Ok(Self::Nvml { index, pci_address }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PciAddress {
    domain: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl FromStr for PciAddress {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        const BAD_ADDRESS: &str = "expected a PCI address like 0000:04:00.0";
        let (domain, rest) = value.split_once(':').ok_or(BAD_ADDRESS)?;
        let (bus, rest) = rest.split_once(':').ok_or(BAD_ADDRESS)?;
        let (device, function) = rest.split_once('.').ok_or(BAD_ADDRESS)?;
        if !matches!(domain.len(), 4 | 8)
            || bus.len() != 2
            || device.len() != 2
            || function.len() != 1
            || ![domain, bus, device, function]
                .iter()
                .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(BAD_ADDRESS);
        }

        let address = Self {
            domain: u16::from_str_radix(domain, 16).map_err(|_| BAD_ADDRESS)?,
            bus: u8::from_str_radix(bus, 16).map_err(|_| BAD_ADDRESS)?,
            device: u8::from_str_radix(device, 16).map_err(|_| BAD_ADDRESS)?,
            function: u8::from_str_radix(function, 16).map_err(|_| BAD_ADDRESS)?,
        };
        if address.device > 31 || address.function > 7 {
            return Err(BAD_ADDRESS);
        }
        Ok(address)
    }
}

impl fmt::Display for PciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

impl<'de> Deserialize<'de> for PciAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid config {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.into(),
            source,
        })?;
        Self::parse(path, &text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source: Box::new(source),
        })?;
        config.validate().map_err(|message| ConfigError::Invalid {
            path: path.into(),
            message,
        })?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if !INTERVAL_RANGE.contains(&self.update_interval_ms) {
            return Err(format!(
                "update_interval_ms must be between {} and {}",
                INTERVAL_RANGE.start(),
                INTERVAL_RANGE.end()
            ));
        }

        for (name, source) in [("cpu", &self.cpu), ("gpu", &self.gpu)] {
            match source {
                Source::Hwmon { chip, sensor, .. } => {
                    validate_selector(name, "chip", chip)?;
                    validate_selector(name, "sensor", sensor)?;
                }
                Source::Nvml {
                    index: Some(_),
                    pci_address: Some(_),
                } => return Err(format!("[{name}] specify either index or pci_address")),
                Source::Nvml { .. } if name == "cpu" => {
                    return Err("[cpu] source \"nvml\" is only available in [gpu]".into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.update_interval_ms)
    }
}

fn validate_selector(section: &str, field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!(
            "[{section}] {field} must be nonempty, without leading or trailing whitespace or control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(Path::new("test-config.toml"), text)
    }

    #[test]
    fn empty_document_defaults_to_automatic() {
        let config = parse("").unwrap();
        assert_eq!(config.interval(), Duration::from_secs(1));
        assert_eq!(config.cpu, Source::Auto);
        assert_eq!(config.gpu, Source::Auto);
    }

    #[test]
    fn accepts_all_sources_and_normalizes_pci() {
        let config = parse(
            r#"
            update_interval_ms = 250
            [cpu]
            source = "hwmon"
            chip = "coretemp"
            sensor = "Package id 0"
            [gpu]
            source = "nvml"
            pci_address = "00000000:7B:00.0"
            "#,
        )
        .unwrap();
        assert_eq!(config.interval(), Duration::from_millis(250));
        assert!(matches!(config.cpu, Source::Hwmon { .. }));
        assert!(matches!(
            config.gpu,
            Source::Nvml { pci_address: Some(address), index: None }
                if address.to_string() == "0000:7b:00.0"
        ));
        assert_eq!(parse("[gpu]\nsource = 'off'").unwrap().gpu, Source::Off);
        assert_eq!(
            parse("[gpu]\nsource = 'nvml'\nindex = 2").unwrap().gpu,
            Source::Nvml {
                index: Some(2),
                pci_address: None
            }
        );
    }

    #[test]
    fn rejects_unknown_keys_types_duplicates_and_wrong_source_fields() {
        for text in [
            "bogus = true",
            "update_interval_ms = '1000'",
            "update_interval_ms = -1",
            "update_interval_ms = 1000\nupdate_interval_ms = 2000",
            "[gpu]\nsource = 'nvidia'",
            "[gpu]\nsource = 'auto'\nindex = 0",
            "[gpu]\nsource = 'off'\nchip = 'amdgpu'",
            "[gpu]\nsource = 'hwmon'\nchip = 'i915'",
            "[gpu]\nsource = 'hwmon'\nchip = 'i915'\nsensor = 'temp1'\nindex = 0",
            "[gpu]\nsource = 'nvml'\nindex = -1",
            "[gpu]\nsource = 'nvml'\nchip = 'nvidia'",
            "[gpu]\nindex = 0",
            "[gpu",
        ] {
            let error = parse(text).unwrap_err();
            assert!(
                matches!(error, ConfigError::Parse { .. }),
                "{text}: {error}"
            );
            assert!(error.to_string().contains("test-config.toml"));
        }
    }

    #[test]
    fn rejects_invalid_semantic_values() {
        for (text, message) in [
            ("update_interval_ms = 0", "between 250 and 60000"),
            ("update_interval_ms = 60001", "between 250 and 60000"),
            ("[cpu]\nsource = 'nvml'", "only available in [gpu]"),
            (
                "[cpu]\nsource = 'hwmon'\nchip = ''\nsensor = 'temp1'",
                "chip must be nonempty",
            ),
            (
                "[gpu]\nsource = 'hwmon'\nchip = 'i915'\nsensor = ' temp1'",
                "sensor must be nonempty",
            ),
            (
                "[gpu]\nsource = 'hwmon'\nchip = 'i915'\nsensor = \"temp1\\n\"",
                "control characters",
            ),
            (
                "[gpu]\nsource = 'nvml'\nindex = 0\npci_address = '0000:04:00.0'",
                "either index or pci_address",
            ),
        ] {
            let error = parse(text).unwrap_err();
            assert!(matches!(error, ConfigError::Invalid { .. }));
            assert!(error.to_string().contains(message), "{error}");
        }
    }

    #[test]
    fn validates_pci_address_widths_and_ranges() {
        for invalid in [
            "04:00.0",
            "0000:4:00.0",
            "0000:04:20.0",
            "0000:04:00.8",
            "0000:04:gg.0",
            "0000:04:00.+",
            "00010000:04:00.0",
            "0000:04:00.00",
        ] {
            assert!(invalid.parse::<PciAddress>().is_err(), "{invalid}");
        }
        let error = parse("[gpu]\nsource = 'nvml'\npci_address = 'bad'").unwrap_err();
        assert!(error.to_string().contains("expected a PCI address"));
        assert!("fffe:ff:1f.7".parse::<PciAddress>().is_ok());
    }

    #[test]
    fn missing_file_reports_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing.toml");
        let error = Config::load(&path).unwrap_err();
        assert!(matches!(error, ConfigError::Read { .. }));
        assert!(error.to_string().contains("missing.toml"));
    }
}
