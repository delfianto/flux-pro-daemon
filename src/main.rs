use std::{path::PathBuf, process::ExitCode, sync::mpsc, time::Instant};

use clap::Parser;
use config::{CONFIG_EXIT_CODE, Config, DEFAULT_PATH};
use sensors::{Reading, Sensors};

mod config;
mod daemon;
mod protocol;
mod sensors;
mod usb;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Use a TOML configuration file at this path.
    #[arg(short, long, value_name = "PATH", default_value = DEFAULT_PATH)]
    config: PathBuf,

    /// Validate the TOML configuration without opening the display.
    #[arg(long, group = "action")]
    check_config: bool,

    /// List temperature sources without opening the display.
    #[arg(long, group = "action")]
    list_sensors: bool,

    /// Print one reading per slot without opening the display.
    #[arg(long, group = "action")]
    read_once: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut sensors = Sensors::default();
    if args.list_sensors {
        sensors.list();
        return ExitCode::SUCCESS;
    }

    let config = match Config::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(CONFIG_EXIT_CODE);
        }
    };
    if args.check_config {
        println!("Valid configuration: {}", args.config.display());
        return ExitCode::SUCCESS;
    }
    if args.read_once {
        let [cpu, gpu] = sensors.read(&config, Instant::now());
        println!("CPU: {cpu}\nGPU: {gpu}");
        return if matches!(cpu, Reading::Unavailable(_)) || matches!(gpu, Reading::Unavailable(_)) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    let (send, stop) = mpsc::sync_channel(1);
    if let Err(error) = ctrlc::set_handler(move || {
        let _ = send.try_send(());
    }) {
        eprintln!("Cannot install shutdown signal handler: {error}");
        return ExitCode::FAILURE;
    }

    daemon::run(&config, &stop);
    ExitCode::SUCCESS
}
