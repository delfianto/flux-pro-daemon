# Flux Pro Daemon

A Linux service for the Antec Flux Pro's USB temperature display (`2022:0522`). The case has two numeric fields labeled CPU and GPU. Each can use a different temperature sensor.

## Arch Linux and CachyOS

From this checkout, build and install the package:

```sh
packaging/arch/build.sh -si
```

The build script stages a checksummed source archive and puts the package in `dist/`. The package preserves edits to `/etc/fluxprod.toml` across upgrades. An upgrade from `0.1.0-1` carries over the settings from the previous config path. Arch's systemd hooks create the service's USB access group and reload systemd and udev. The package's own hook restarts an already-running service after those hooks. Start the service when the configuration is ready:

```sh
fluxprod --check-config
fluxprod --read-once
sudo systemctl enable --now antec-flux-pro-daemon.service
journalctl -u antec-flux-pro-daemon.service -b
```

The system service runs as a dynamic, unprivileged user with access to the display through the `antec-flux-pro` group. The udev rule also grants USB access to the active local desktop session. On a machine with a previous Flux Pro service, stop that service before starting this one so only one process claims the USB interface.

## Choose the readings

Run `fluxprod --list-sensors` to see available temperature inputs and PCI addresses. The packaged defaults are `source = "auto"` for both fields and a 1,000 ms interval.

```toml
update_interval_ms = 1000

[cpu]
source = "hwmon"
chip = "k10temp"
sensor = "Tctl"

[gpu]
source = "hwmon"
chip = "i915"
sensor = "temp1"
pci_address = "0000:04:00.0"
```

- `hwmon` uses an exact, case-insensitive chip name and sensor label or channel name such as `temp1`. Use the optional `pci_address` when two devices have the same chip name and sensor. Any exposed hwmon temperature, including an SSD or motherboard temperature, can fill either field.
- `nvml` is available for the GPU field. Set `[gpu]` with `source = "nvml"` and optionally specify `index = 0` or `pci_address = "0000:01:00.0"`. Without either selector, it uses the first NVIDIA GPU with a readable temperature. Install the NVIDIA driver and its userspace package, typically `nvidia-utils` on Arch, to supply `libnvidia-ml.so.1`.
- `auto` looks for package temperatures from AMD `k10temp`/`zenpower` or Intel `coretemp`, with other recognized CPU hwmon drivers as fallbacks. For the GPU it tries NVML, followed by NVIDIA/Nouveau, Intel `xe`/`i915`, AMD `amdgpu`/`radeon`, and GPU thermal hwmon sensors. Set an explicit source to choose a specific GPU in a multi-GPU system.
- `off` blanks that field.

`update_interval_ms` accepts integers from 250 to 60,000. `--check-config` validates TOML and selector syntax without touching USB. `--read-once` also checks whether the selected sensors can be read, without opening the display. Use `--config PATH` for either command to inspect a file outside `/etc`.

The daemon blanks a field while its sensor is unavailable or outside the panel's 0.0–99.9 °C range. It retries unavailable devices automatically. Restart the service to apply config changes.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo run --locked -- --config packaging/fluxprod.toml --read-once
```

The project uses Rust 1.100 and the 2024 edition. CI also builds and smoke-tests the Arch package and uploads it as a workflow artifact.

USB packet format documented by [Nish Tahir](https://nishtahir.com/building-an-ubuntu-service-for-my-antec-flux-display/).
