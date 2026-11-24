use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::{config::PciAddress, protocol::Temperature};

pub const ROOT: &str = "/sys/class/hwmon";

#[derive(Debug)]
pub struct Sensor {
    pub chip: String,
    pub label: Option<String>,
    pub pci: Option<PciAddress>,
    channel: u32,
    input: PathBuf,
}

impl Sensor {
    fn channel_name(&self) -> String {
        format!("temp{}", self.channel)
    }

    fn matches(&self, chip: &str, sensor: &str, pci: Option<PciAddress>) -> bool {
        self.chip.eq_ignore_ascii_case(chip)
            && pci.is_none_or(|address| self.pci == Some(address))
            && (self.channel_name().eq_ignore_ascii_case(sensor)
                || self
                    .label
                    .as_ref()
                    .is_some_and(|label| label.eq_ignore_ascii_case(sensor)))
    }

    pub fn read(&self) -> Result<Temperature, String> {
        let raw = fs::read_to_string(&self.input)
            .map_err(|error| format!("cannot read {}: {error}", self.input.display()))?;
        let value = raw.trim().parse::<i64>().map_err(|_| {
            format!(
                "{} returned invalid millidegrees {:?}",
                self.input.display(),
                raw.trim()
            )
        })?;
        Temperature::from_millidegrees(value).ok_or_else(|| {
            format!("{self} reported {value} millidegrees, outside the display range")
        })
    }
}

impl fmt::Display for Sensor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.chip, self.channel_name())?;
        if let Some(label) = &self.label {
            write!(f, " ({label})")?;
        }
        if let Some(address) = self.pci {
            write!(f, " [{address}]")?;
        }
        Ok(())
    }
}

pub fn scan(root: &Path) -> io::Result<Vec<Sensor>> {
    let mut sensors = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        let chip = name.trim();
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        let pci = fs::canonicalize(path.join("device"))
            .ok()
            .or_else(|| fs::canonicalize(&path).ok())
            .and_then(|path| {
                path.ancestors()
                    .find_map(|parent| parent.file_name()?.to_str()?.parse().ok())
            });

        for file in files.flatten() {
            let name = file.file_name();
            let Some(channel) = name.to_str().and_then(temperature_channel) else {
                continue;
            };
            let label = fs::read_to_string(path.join(format!("temp{channel}_label")))
                .ok()
                .map(|label| label.trim().to_owned())
                .filter(|label| !label.is_empty());
            sensors.push(Sensor {
                chip: chip.to_owned(),
                label,
                pci,
                channel,
                input: file.path(),
            });
        }
    }
    sensors.sort_by(|a, b| {
        (&a.chip, a.pci, a.channel, &a.input).cmp(&(&b.chip, b.pci, b.channel, &b.input))
    });
    Ok(sensors)
}

fn temperature_channel(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("temp")?.strip_suffix("_input")?;
    if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub fn select<'a>(
    sensors: &'a [Sensor],
    chip: &str,
    sensor: &str,
    pci: Option<PciAddress>,
) -> Result<&'a Sensor, String> {
    let mut matches = sensors
        .iter()
        .filter(|entry| entry.matches(chip, sensor, pci));
    let describe = || match pci {
        Some(address) => format!("{chip} {sensor} at {address}"),
        None => format!("{chip} {sensor}"),
    };
    let selected = matches
        .next()
        .ok_or_else(|| format!("no hwmon temperature matches {}", describe()))?;
    if let Some(other) = matches.next() {
        return Err(format!(
            "ambiguous hwmon temperature {} ({selected}, {other}); specify a PCI address or a unique tempN channel",
            describe()
        ));
    }
    Ok(selected)
}

#[derive(Clone, Copy)]
pub enum Slot {
    Cpu,
    Gpu,
}

pub fn auto(sensors: &[Sensor], slot: Slot) -> Result<(&Sensor, Temperature), String> {
    let rank = match slot {
        Slot::Cpu => cpu_rank,
        Slot::Gpu => gpu_rank,
    };
    let mut candidates: Vec<_> = sensors
        .iter()
        .filter_map(|sensor| Some((rank(sensor)?, sensor)))
        .collect();
    candidates.sort_by_key(|(rank, _)| *rank);
    let mut last_error = None;
    for (_, sensor) in candidates {
        match sensor.read() {
            Ok(value) => return Ok((sensor, value)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| match slot {
        Slot::Cpu => "no supported CPU temperature in hwmon".into(),
        Slot::Gpu => "no supported GPU temperature in hwmon".into(),
    }))
}

fn cpu_rank(sensor: &Sensor) -> Option<(u8, u8)> {
    let label = sensor.label.as_deref().unwrap_or_default();
    match (sensor.chip.as_str(), label) {
        ("k10temp" | "zenpower", "Tdie") => Some((0, 0)),
        ("k10temp" | "zenpower", "Tctl") => Some((0, 1)),
        ("coretemp", label) if label.starts_with("Package id ") => Some((1, 0)),
        (
            "coretemp" | "k8temp" | "k10temp" | "zenpower" | "cpu_thermal" | "cpu-thermal"
            | "x86_pkg_temp" | "acpitz",
            _,
        ) if sensor.channel == 1 => Some((2, 0)),
        _ => None,
    }
}

fn gpu_rank(sensor: &Sensor) -> Option<(u8, u8)> {
    let driver = match sensor.chip.as_str() {
        "nvidia" | "nouveau" => 0,
        "xe" | "i915" => 1,
        "amdgpu" | "radeon" => 2,
        "gpu_thermal" | "gpu-thermal" => 3,
        _ => return None,
    };
    let channel = match sensor.label.as_deref() {
        Some("edge" | "GPU Core") => 0,
        _ if sensor.channel == 1 => 1,
        Some("junction") => 2,
        _ => return None,
    };
    Some((driver, channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn chip(root: &Path, index: u32, name: &str, address: Option<&str>, files: &[(&str, &str)]) {
        let path = root.join(format!("hwmon{index}"));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("name"), name).unwrap();
        if let Some(address) = address {
            let device = root.join(address);
            if !device.exists() {
                fs::create_dir(&device).unwrap();
            }
            symlink(device, path.join("device")).unwrap();
        }
        for (name, data) in files {
            fs::write(path.join(name), data).unwrap();
        }
    }

    #[test]
    fn selects_only_temperature_inputs_by_exact_label_or_channel() {
        let tmp = tempfile::tempdir().unwrap();
        chip(
            tmp.path(),
            4,
            "i915\n",
            Some("0000:04:00.0"),
            &[
                ("temp1_input", "45250\n"),
                ("temp1_label", "GPU Core\n"),
                ("temp10_input", "33000"),
                ("temp1_max", "90000"),
                ("fan1_input", "1500"),
                ("in1_input", "900"),
                ("temp2_crit", "100000"),
                ("temp01_input", "999"),
            ],
        );
        let sensors = scan(tmp.path()).unwrap();
        assert_eq!(sensors.len(), 2);
        assert_eq!(
            select(&sensors, "I915", "gpu core", None)
                .unwrap()
                .read()
                .unwrap()
                .to_string(),
            "45.3 °C"
        );
        assert_eq!(
            select(&sensors, "i915", "temp10", None).unwrap().channel,
            10
        );
        for (chip, label) in [
            ("i91", "temp1"),
            ("i915", "gpu"),
            ("i915", "fan1"),
            ("i915", "temp"),
        ] {
            assert!(select(&sensors, chip, label, None).is_err());
        }
    }

    #[test]
    fn ambiguity_requires_a_stable_pci_address() {
        let tmp = tempfile::tempdir().unwrap();
        let files = [("temp1_input", "42000"), ("temp1_label", "edge")];
        chip(tmp.path(), 0, "amdgpu", Some("0000:03:00.0"), &files);
        chip(tmp.path(), 9, "amdgpu", Some("0000:7b:00.0"), &files);
        let sensors = scan(tmp.path()).unwrap();
        assert!(
            select(&sensors, "amdgpu", "edge", None)
                .unwrap_err()
                .contains("ambiguous")
        );
        let address = "0000:7b:00.0".parse().unwrap();
        let sensor = select(&sensors, "amdgpu", "edge", Some(address)).unwrap();
        assert_eq!(sensor.pci, Some(address));
        assert!(sensor.input.ends_with("hwmon9/temp1_input"));
        assert!(
            select(
                &sensors,
                "amdgpu",
                "edge",
                Some("0000:05:00.0".parse().unwrap())
            )
            .is_err()
        );
    }

    #[test]
    fn auto_skips_unreadable_candidates_and_prefers_package_sensors() {
        let tmp = tempfile::tempdir().unwrap();
        chip(
            tmp.path(),
            0,
            "k10temp",
            None,
            &[
                ("temp1_input", "47000"),
                ("temp1_label", "Tctl"),
                ("temp2_input", "corrupt"),
                ("temp2_label", "Tdie"),
                ("temp3_input", "34000"),
                ("temp3_label", "Tccd1"),
            ],
        );
        chip(
            tmp.path(),
            1,
            "coretemp",
            None,
            &[("temp1_input", "46000"), ("temp1_label", "Package id 0")],
        );
        chip(tmp.path(), 2, "nvme", None, &[("temp1_input", "38000")]);
        let sensors = scan(tmp.path()).unwrap();
        let (sensor, value) = auto(&sensors, Slot::Cpu).unwrap();
        assert_eq!(sensor.label.as_deref(), Some("Tctl"));
        assert_eq!(value.to_string(), "47.0 °C");
        assert!(auto(&sensors, Slot::Gpu).is_err());
    }

    #[test]
    fn fresh_scan_tracks_renumbering_and_missing_hardware() {
        let tmp = tempfile::tempdir().unwrap();
        chip(
            tmp.path(),
            1,
            "i915",
            Some("0000:04:00.0"),
            &[("temp1_input", "99001")],
        );
        chip(
            tmp.path(),
            2,
            "amdgpu",
            Some("0000:7b:00.0"),
            &[("temp1_input", "43000")],
        );
        let initial = scan(tmp.path()).unwrap();
        assert_eq!(auto(&initial, Slot::Gpu).unwrap().0.chip, "i915");
        fs::rename(tmp.path().join("hwmon1"), tmp.path().join("hwmon5")).unwrap();
        assert!(
            initial
                .iter()
                .find(|s| s.chip == "i915")
                .unwrap()
                .read()
                .is_err()
        );
        let after = scan(tmp.path()).unwrap();
        assert!(
            auto(&after, Slot::Gpu)
                .unwrap()
                .0
                .input
                .ends_with("hwmon5/temp1_input")
        );
        fs::remove_dir_all(tmp.path().join("hwmon5")).unwrap();
        assert_eq!(
            auto(&scan(tmp.path()).unwrap(), Slot::Gpu).unwrap().0.chip,
            "amdgpu"
        );
    }

    #[test]
    fn invalid_and_out_of_range_reads_are_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        chip(
            tmp.path(),
            0,
            "i915",
            None,
            &[("temp1_input", "NaN"), ("temp2_input", "100000")],
        );
        let sensors = scan(tmp.path()).unwrap();
        assert!(
            sensors[0]
                .read()
                .unwrap_err()
                .contains("invalid millidegrees")
        );
        assert!(
            sensors[1]
                .read()
                .unwrap_err()
                .contains("outside the display range")
        );
    }
}
