use std::{
    ffi::OsStr,
    time::{Duration, Instant},
};

use nvml_wrapper::{Device, Nvml, enum_wrappers::device::TemperatureSensor};

use crate::{config::PciAddress, protocol::Temperature};

const RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Monitor {
    nvml: Option<Nvml>,
    last_error: Option<(Instant, String)>,
}

impl Monitor {
    fn connect(
        &mut self,
        now: Instant,
        init: impl FnOnce() -> Result<Nvml, String>,
    ) -> Result<&Nvml, String> {
        if let Some((retry_at, error)) = &self.last_error
            && now < *retry_at
        {
            return Err(error.clone());
        }
        let slot = match &mut self.nvml {
            Some(nvml) => return Ok(nvml),
            slot => slot,
        };
        match init() {
            Ok(nvml) => {
                self.last_error = None;
                Ok(slot.insert(nvml))
            }
            Err(error) => {
                self.last_error = Some((now + RETRY_INTERVAL, error.clone()));
                Err(error)
            }
        }
    }

    pub fn read(
        &mut self,
        now: Instant,
        index: Option<u32>,
        address: Option<PciAddress>,
    ) -> Result<(Temperature, String), String> {
        let nvml = self.connect(now, initialize)?;
        let reading = read_gpu(nvml, index, address);
        if let Err(error) = &reading {
            self.nvml = None;
            self.last_error = Some((now + RETRY_INTERVAL, error.clone()));
        }
        reading
    }

    pub fn list(&mut self, now: Instant) -> Result<Vec<String>, String> {
        let nvml = self.connect(now, initialize)?;
        let count = nvml
            .device_count()
            .map_err(|error| format!("cannot enumerate NVIDIA GPUs: {error}"))?;
        Ok((0..count)
            .map(|index| match nvml.device_by_index(index) {
                Ok(gpu) => {
                    let name = gpu.name().unwrap_or_else(|_| "NVIDIA GPU".into());
                    let address = gpu
                        .pci_info()
                        .ok()
                        .and_then(|info| info.bus_id.parse::<PciAddress>().ok())
                        .map(|pci| format!(" [{pci}]"))
                        .unwrap_or_default();
                    let reading = read_device(&gpu).map_or_else(|e| e, |temp| temp.to_string());
                    format!("nvml index={index} {name}{address}: {reading}")
                }
                Err(error) => format!("nvml index={index}: {error}"),
            })
            .collect())
    }
}

fn initialize() -> Result<Nvml, String> {
    Nvml::builder()
        .lib_path(OsStr::new("libnvidia-ml.so.1"))
        .init()
        .map_err(|error| format!("NVML unavailable (libnvidia-ml.so.1): {error}"))
}

fn read_gpu(
    nvml: &Nvml,
    index: Option<u32>,
    address: Option<PciAddress>,
) -> Result<(Temperature, String), String> {
    if let Some(address) = address {
        let gpu = nvml
            .device_by_pci_bus_id(address.to_string())
            .map_err(|error| format!("cannot open NVIDIA GPU at {address}: {error}"))?;
        return read_device(&gpu).map(|value| (value, format!("NVML {address}")));
    }
    if let Some(index) = index {
        return read_index(nvml, index);
    }
    let count = nvml
        .device_count()
        .map_err(|error| format!("cannot enumerate NVIDIA GPUs: {error}"))?;
    let mut last_error = "NVML found no NVIDIA GPUs".to_owned();
    for index in 0..count {
        match read_index(nvml, index) {
            Ok(reading) => return Ok(reading),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn read_index(nvml: &Nvml, index: u32) -> Result<(Temperature, String), String> {
    let gpu = nvml
        .device_by_index(index)
        .map_err(|error| format!("cannot open NVIDIA GPU {index}: {error}"))?;
    let source = gpu.pci_info().map_or_else(
        |_| format!("NVML index {index}"),
        |info| format!("NVML {}", info.bus_id),
    );
    read_device(&gpu)
        .map(|value| (value, source))
        .map_err(|error| format!("NVIDIA GPU {index}: {error}"))
}

fn read_device(gpu: &Device<'_>) -> Result<Temperature, String> {
    let celsius = gpu
        .temperature(TemperatureSensor::Gpu)
        .map_err(|error| format!("cannot read GPU temperature: {error}"))?;
    Temperature::from_degrees(celsius)
        .ok_or_else(|| format!("GPU reported {celsius} °C, outside the display range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_driver_is_rate_limited_and_retried() {
        let now = Instant::now();
        let mut monitor = Monitor::default();
        assert!(
            matches!(monitor.connect(now, || Err("driver missing".into())), Err(error) if error == "driver missing")
        );
        assert!(
            matches!(monitor.connect(now + RETRY_INTERVAL - Duration::from_millis(1), || panic!("retried too soon")), Err(error) if error == "driver missing")
        );
        assert!(
            matches!(monitor.connect(now + RETRY_INTERVAL, || Err("driver still missing".into())), Err(error) if error == "driver still missing")
        );
    }
}
