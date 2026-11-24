use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};

use crate::{
    config::Config,
    protocol::{self, PACKET_SIZE},
    sensors::Sensors,
    usb,
};

const USB_RETRY: Duration = Duration::from_secs(2);

trait DisplayOutput {
    fn write(&mut self, bytes: &[u8; PACKET_SIZE]) -> Result<(), String>;
}

impl DisplayOutput for usb::Display {
    fn write(&mut self, bytes: &[u8; PACKET_SIZE]) -> Result<(), String> {
        self.write(bytes).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Eq, PartialEq)]
enum DisplayEvent {
    Connected,
    Unavailable(String),
}

struct Connection<D> {
    display: Option<D>,
    retry_at: Option<Instant>,
    previous_error: Option<String>,
}

impl<D: DisplayOutput> Connection<D> {
    const fn new() -> Self {
        Self {
            display: None,
            retry_at: None,
            previous_error: None,
        }
    }

    fn update(
        &mut self,
        now: Instant,
        bytes: &[u8; PACKET_SIZE],
        connect: impl FnOnce() -> Result<D, String>,
    ) -> Option<DisplayEvent> {
        let reconnect = self.display.is_none();
        if self.retry_at.is_some_and(|retry_at| now < retry_at) {
            return None;
        }

        let result = match &mut self.display {
            Some(display) => display.write(bytes),
            slot @ None => connect().and_then(|mut display| {
                display.write(bytes)?;
                *slot = Some(display);
                Ok(())
            }),
        };
        match result {
            Ok(()) => {
                self.retry_at = None;
                self.previous_error = None;
                reconnect.then_some(DisplayEvent::Connected)
            }
            Err(error) => {
                self.display = None;
                self.retry_at = Some(now + USB_RETRY);
                changed(&mut self.previous_error, &error)
                    .then_some(DisplayEvent::Unavailable(error))
            }
        }
    }

    fn blank(&mut self) -> Result<(), String> {
        if let Some(mut display) = self.display.take() {
            display.write(&protocol::packet(None, None))?;
        }
        Ok(())
    }
}

fn changed(previous: &mut Option<String>, value: &str) -> bool {
    if previous.as_deref() == Some(value) {
        return false;
    }
    *previous = Some(value.to_owned());
    true
}

pub fn run(config: &Config, stop: &Receiver<()>) {
    let mut sensors = Sensors::default();
    let mut display = Connection::new();
    let mut status = [None, None];
    let interval = config.interval();

    while matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
        let now = Instant::now();
        let [cpu, gpu] = sensors.read(config, now);
        for ((label, reading), previous) in
            [("CPU", &cpu), ("GPU", &gpu)].into_iter().zip(&mut status)
        {
            if changed(previous, reading.status()) {
                eprintln!("{label}: {reading}");
            }
        }
        let bytes = protocol::packet(cpu.temperature(), gpu.temperature());
        match display.update(now, &bytes, || {
            usb::Display::open().map_err(|error| error.to_string())
        }) {
            Some(DisplayEvent::Connected) => eprintln!("Flux Pro display connected"),
            Some(DisplayEvent::Unavailable(error)) => {
                eprintln!("Flux Pro display unavailable: {error}. Retrying.")
            }
            None => {}
        }

        let remaining = interval.saturating_sub(now.elapsed());
        if !matches!(stop.recv_timeout(remaining), Err(RecvTimeoutError::Timeout)) {
            break;
        }
    }

    if let Err(error) = display.blank() {
        eprintln!("Could not blank the display on shutdown: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Clone, Default)]
    struct FakeDisplay {
        frames: Rc<RefCell<Vec<[u8; PACKET_SIZE]>>>,
        writes: Rc<RefCell<VecDeque<Result<(), String>>>>,
    }

    impl DisplayOutput for FakeDisplay {
        fn write(&mut self, bytes: &[u8; PACKET_SIZE]) -> Result<(), String> {
            self.frames.borrow_mut().push(*bytes);
            self.writes.borrow_mut().pop_front().unwrap_or(Ok(()))
        }
    }

    #[test]
    fn keeps_connection_open_and_writes_only_one_blank_on_shutdown() {
        let now = Instant::now();
        let mut connection = Connection::new();
        let output = FakeDisplay::default();
        let frame = protocol::packet(protocol::Temperature::from_degrees(44), None);
        assert_eq!(
            connection.update(now, &frame, || Ok(output.clone())),
            Some(DisplayEvent::Connected)
        );
        assert_eq!(
            connection.update(now, &frame, || panic!("reopened an active device")),
            None
        );
        connection.blank().unwrap();
        connection.blank().unwrap();
        assert_eq!(
            *output.frames.borrow(),
            [frame, frame, protocol::packet(None, None)]
        );
    }

    #[test]
    fn write_failure_drops_handle_and_reconnects_after_backoff() {
        let now = Instant::now();
        let mut connection = Connection::new();
        let first = FakeDisplay::default();
        first
            .writes
            .borrow_mut()
            .extend([Ok(()), Err("unplugged".into())]);
        let second = FakeDisplay::default();
        let frame = protocol::packet(None, None);
        assert_eq!(
            connection.update(now, &frame, || Ok(first.clone())),
            Some(DisplayEvent::Connected)
        );
        assert_eq!(
            connection.update(now, &frame, || panic!("must use existing handle")),
            Some(DisplayEvent::Unavailable("unplugged".into()))
        );
        assert!(connection.display.is_none());
        assert_eq!(
            connection.update(now + Duration::from_secs(1), &frame, || panic!(
                "retried too early"
            )),
            None
        );
        assert_eq!(
            connection.update(now + USB_RETRY, &frame, || Ok(second.clone())),
            Some(DisplayEvent::Connected)
        );
        assert_eq!(*first.frames.borrow(), [frame, frame]);
        assert_eq!(*second.frames.borrow(), [frame]);
    }

    #[test]
    fn repeated_open_error_reports_once_but_retries() {
        let now = Instant::now();
        let mut connection: Connection<FakeDisplay> = Connection::new();
        let frame = protocol::packet(None, None);
        assert_eq!(
            connection.update(now, &frame, || Err("absent".into())),
            Some(DisplayEvent::Unavailable("absent".into()))
        );
        assert_eq!(
            connection.update(now + USB_RETRY, &frame, || Err("absent".into())),
            None
        );
        assert_eq!(
            connection.update(now + 2 * USB_RETRY, &frame, || Err("access denied".into())),
            Some(DisplayEvent::Unavailable("access denied".into()))
        );
        connection.blank().unwrap();
    }

    #[test]
    fn a_failed_first_write_is_reported_as_unavailable() {
        let now = Instant::now();
        let mut connection = Connection::new();
        let output = FakeDisplay::default();
        output.writes.borrow_mut().push_back(Err("stall".into()));
        let frame = protocol::packet(None, None);
        assert_eq!(
            connection.update(now, &frame, || Ok(output)),
            Some(DisplayEvent::Unavailable("stall".into()))
        );
        assert!(connection.display.is_none());
    }
}
