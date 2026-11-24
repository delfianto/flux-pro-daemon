use std::time::Duration;

use nusb::{
    MaybeFuture,
    descriptors::{ConfigurationDescriptor, TransferType},
    transfer::{Buffer, Direction, Interrupt, Out},
};
use thiserror::Error;

use crate::protocol::PACKET_SIZE;

const VENDOR_ID: u16 = 0x2022;
const PRODUCT_ID: u16 = 0x0522;
const PREFERRED_ENDPOINT: (u8, u8) = (0, 0x03);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum UsbError {
    #[error("cannot enumerate USB devices: {0}")]
    Enumerate(nusb::Error),
    #[error("Antec Flux Pro USB display (2022:0522) is absent")]
    Missing,
    #[error("multiple Antec Flux Pro USB displays (2022:0522) are connected")]
    MultipleDisplays,
    #[error("cannot open USB display 2022:0522: {0}; check its udev permissions")]
    Open(nusb::Error),
    #[error("cannot read the display's active USB configuration: {0}")]
    Configuration(nusb::ActiveConfigurationError),
    #[error("display has no usable interrupt OUT endpoint")]
    NoEndpoint,
    #[error("display has multiple interrupt OUT endpoints and no recognized endpoint")]
    AmbiguousEndpoint,
    #[error("cannot claim display USB interface {interface}: {error}")]
    Claim { interface: u8, error: nusb::Error },
    #[error("cannot open display USB endpoint 0x{address:02x}: {error}")]
    Endpoint { address: u8, error: nusb::Error },
    #[error("display interrupt write timed out")]
    Timeout,
    #[error("display interrupt write failed: {0}")]
    Transfer(nusb::transfer::TransferError),
    #[error("short display interrupt write: sent {0} of {PACKET_SIZE} bytes")]
    ShortWrite(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Endpoint {
    interface: u8,
    address: u8,
    capacity: usize,
}

pub struct Display {
    endpoint: nusb::Endpoint<Interrupt, Out>,
}

impl Display {
    pub fn open() -> Result<Self, UsbError> {
        let mut matching = nusb::list_devices()
            .wait()
            .map_err(UsbError::Enumerate)?
            .filter(|device| device.vendor_id() == VENDOR_ID && device.product_id() == PRODUCT_ID);
        let info = matching.next().ok_or(UsbError::Missing)?;
        if matching.next().is_some() {
            return Err(UsbError::MultipleDisplays);
        }
        let device = info.open().wait().map_err(UsbError::Open)?;
        let config = device
            .active_configuration()
            .map_err(UsbError::Configuration)?;
        let Endpoint {
            interface, address, ..
        } = display_endpoint(config)?;
        let claimed = device
            .detach_and_claim_interface(interface)
            .wait()
            .map_err(|error| UsbError::Claim { interface, error })?;
        let endpoint = claimed
            .endpoint::<Interrupt, Out>(address)
            .map_err(|error| UsbError::Endpoint { address, error })?;
        Ok(Self { endpoint })
    }

    pub fn write(&mut self, frame: &[u8; PACKET_SIZE]) -> Result<(), UsbError> {
        let mut buffer = Buffer::new(PACKET_SIZE);
        buffer.extend_from_slice(frame);
        self.endpoint.submit(buffer);
        let completion = self
            .endpoint
            .wait_next_complete(WRITE_TIMEOUT)
            .ok_or(UsbError::Timeout)?;
        completion.status.map_err(UsbError::Transfer)?;
        if completion.actual_len != PACKET_SIZE {
            return Err(UsbError::ShortWrite(completion.actual_len));
        }
        Ok(())
    }
}

fn display_endpoint(config: ConfigurationDescriptor<'_>) -> Result<Endpoint, UsbError> {
    let endpoints = config
        .interface_alt_settings()
        .filter(|interface| interface.alternate_setting() == 0)
        .flat_map(|interface| {
            let number = interface.interface_number();
            interface
                .endpoints()
                .filter(|endpoint| {
                    endpoint.direction() == Direction::Out
                        && endpoint.transfer_type() == TransferType::Interrupt
                })
                .map(move |endpoint| Endpoint {
                    interface: number,
                    address: endpoint.address(),
                    capacity: endpoint.max_packet_size(),
                })
        });
    select_endpoint(endpoints)
}

fn select_endpoint(candidates: impl IntoIterator<Item = Endpoint>) -> Result<Endpoint, UsbError> {
    let mut viable = candidates
        .into_iter()
        .filter(|candidate| candidate.capacity >= PACKET_SIZE);
    let first = viable.next().ok_or(UsbError::NoEndpoint)?;
    let preferred =
        |candidate: &Endpoint| (candidate.interface, candidate.address) == PREFERRED_ENDPOINT;
    if preferred(&first) {
        return Ok(first);
    }
    if let Some(second) = viable.next() {
        return std::iter::once(second)
            .chain(viable)
            .find(preferred)
            .ok_or(UsbError::AmbiguousEndpoint);
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flux_pro_descriptor_requires_an_interrupt_out_endpoint() {
        // Configuration, interface, HID, and endpoint descriptors from device 2022:0522.
        let mut data = [
            9, 2, 34, 0, 1, 1, 0, 0xa0, 0x32, 9, 4, 0, 0, 1, 3, 0, 0, 0, 9, 0x21, 0x10, 1, 0, 1,
            0x22, 0x22, 0, 7, 5, 3, 3, 64, 0, 1,
        ];
        let endpoint = display_endpoint(ConfigurationDescriptor::new(&data).unwrap()).unwrap();
        assert_eq!(
            (endpoint.interface, endpoint.address, endpoint.capacity),
            (0, 3, 64)
        );

        data[30] = 2; // Bulk OUT
        assert!(matches!(
            display_endpoint(ConfigurationDescriptor::new(&data).unwrap()),
            Err(UsbError::NoEndpoint)
        ));
        data[30] = 3;
        data[29] = 0x83; // Interrupt IN
        assert!(matches!(
            display_endpoint(ConfigurationDescriptor::new(&data).unwrap()),
            Err(UsbError::NoEndpoint)
        ));
    }

    #[test]
    fn chooses_known_endpoint_even_when_it_is_not_first() {
        let other = Endpoint {
            interface: 0,
            address: 1,
            capacity: 64,
        };
        let preferred = Endpoint {
            interface: 0,
            address: 3,
            capacity: 64,
        };
        assert_eq!(select_endpoint([other, preferred]).unwrap(), preferred);
    }

    #[test]
    fn accepts_single_compatible_endpoint_on_another_interface() {
        let small = Endpoint {
            interface: 0,
            address: 3,
            capacity: 8,
        };
        let usable = Endpoint {
            interface: 1,
            address: 4,
            capacity: 32,
        };
        assert_eq!(select_endpoint([small, usable]).unwrap(), usable);
        assert!(matches!(
            select_endpoint([small]),
            Err(UsbError::NoEndpoint)
        ));
    }

    #[test]
    fn rejects_unknown_ambiguous_arrangements() {
        let a = Endpoint {
            interface: 0,
            address: 1,
            capacity: 64,
        };
        let b = Endpoint { address: 2, ..a };
        assert!(matches!(
            select_endpoint([a, b]),
            Err(UsbError::AmbiguousEndpoint)
        ));
        assert!(matches!(select_endpoint([]), Err(UsbError::NoEndpoint)));
    }
}
