mod hwmon;
mod nvidia;

use std::{fmt, path::Path, time::Instant};

use crate::{
    config::{Config, Source},
    protocol::Temperature,
};
use hwmon::{Sensor, Slot};
use nvidia::Monitor;

pub enum Reading {
    Available(Temperature, String),
    Off,
    Unavailable(String),
}

impl Reading {
    pub fn temperature(&self) -> Option<Temperature> {
        match self {
            Self::Available(temperature, _) => Some(*temperature),
            Self::Off | Self::Unavailable(_) => None,
        }
    }

    pub fn status(&self) -> &str {
        match self {
            Self::Available(_, source) | Self::Unavailable(source) => source,
            Self::Off => "off",
        }
    }
}

impl fmt::Display for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available(value, source) => write!(f, "{value} ({source})"),
            Self::Off => f.write_str("off"),
            Self::Unavailable(error) => write!(f, "unavailable ({error})"),
        }
    }
}

impl From<Result<(Temperature, String), String>> for Reading {
    fn from(result: Result<(Temperature, String), String>) -> Self {
        match result {
            Ok((value, source)) => Self::Available(value, source),
            Err(error) => Self::Unavailable(error),
        }
    }
}

#[derive(Default)]
pub struct Sensors {
    nvidia: Monitor,
}

impl Sensors {
    pub fn read(&mut self, config: &Config, now: Instant) -> [Reading; 2] {
        self.read_at(config, now, Path::new(hwmon::ROOT))
    }

    fn read_at(&mut self, config: &Config, now: Instant, root: &Path) -> [Reading; 2] {
        let hwmon =
            hwmon::scan(root).map_err(|error| format!("cannot scan {}: {error}", root.display()));
        let inventory = hwmon.as_deref().map_err(String::as_str);
        [
            self.read_slot(&config.cpu, Slot::Cpu, inventory, now),
            self.read_slot(&config.gpu, Slot::Gpu, inventory, now),
        ]
    }

    fn read_slot(
        &mut self,
        source: &Source,
        slot: Slot,
        inventory: Result<&[Sensor], &str>,
        now: Instant,
    ) -> Reading {
        let result = match source {
            Source::Off => return Reading::Off,
            Source::Nvml { index, pci_address } => self.nvidia.read(now, *index, *pci_address),
            Source::Hwmon {
                chip,
                sensor,
                pci_address,
            } => inventory
                .map_err(str::to_owned)
                .and_then(|sensors| hwmon::select(sensors, chip, sensor, *pci_address))
                .and_then(|sensor| Ok((sensor.read()?, sensor.to_string()))),
            Source::Auto => {
                let read_hwmon = || {
                    let (sensor, value) = hwmon::auto(inventory?, slot)?;
                    Ok((value, sensor.to_string()))
                };
                match slot {
                    Slot::Gpu => self.nvidia.read(now, None, None).or_else(|nvidia_error| {
                        read_hwmon()
                            .map_err(|hwmon_error: String| format!("{hwmon_error}; {nvidia_error}"))
                    }),
                    Slot::Cpu => read_hwmon(),
                }
            }
        };
        result.into()
    }

    pub fn list(&mut self) {
        match hwmon::scan(Path::new(hwmon::ROOT)) {
            Ok(sensors) => {
                for sensor in sensors {
                    let reading = sensor
                        .read()
                        .map_or_else(|error| error, |value| value.to_string());
                    println!("hwmon {sensor}: {reading}");
                }
            }
            Err(error) => eprintln!("cannot scan {}: {error}", hwmon::ROOT),
        }
        match self.nvidia.list(Instant::now()) {
            Ok(entries) if entries.is_empty() => println!("NVML found no NVIDIA GPUs"),
            Ok(entries) => entries.iter().for_each(|entry| println!("{entry}")),
            Err(error) => println!("{error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn unavailable_slot_does_not_blank_the_other_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "[gpu]\nsource = 'hwmon'\nchip = 'i915'\nsensor = 'temp1'\n",
        )
        .unwrap();
        let chip = tmp.path().join("hwmon0");
        fs::create_dir(&chip).unwrap();
        fs::write(chip.join("name"), "i915").unwrap();
        fs::write(chip.join("temp1_input"), "42000").unwrap();
        let config = Config::load(&config_path).unwrap();
        let [cpu, gpu] = Sensors::default().read_at(&config, Instant::now(), tmp.path());
        assert!(matches!(cpu, Reading::Unavailable(_)));
        assert_eq!(gpu.temperature(), Temperature::from_degrees(42));
    }

    #[test]
    fn off_is_distinct_from_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "[gpu]\nsource = 'off'\n").unwrap();
        let config = Config::load(&config_path).unwrap();
        let [cpu, gpu] =
            Sensors::default().read_at(&config, Instant::now(), &tmp.path().join("missing"));
        assert!(matches!(cpu, Reading::Unavailable(_)));
        assert!(matches!(gpu, Reading::Off));
        assert_eq!(gpu.temperature(), None);
    }
}
