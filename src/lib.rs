mod cli;
mod connect;
mod device;
mod doctor;
mod eject;
mod ls;
mod midi;
mod mtp_session;
mod output;
mod pull;
mod push;
mod remote;
mod stat;
mod status;
mod tree;
mod usb_owner;
mod write_ops;

pub use output::AppError;

use clap::Parser;
use cli::{Cli, Command};

pub fn run() -> Result<(), AppError> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    match &cli.command {
        Command::Devices => {
            let devices = device::list_tp7_devices()?;
            let devices = device::filter_by_serial(devices, cli.device.as_deref())?;
            output::write_devices(&devices, cli.json)
        }
        Command::Doctor => {
            let report = doctor::run_doctor(cli.device.as_deref())?;
            output::write_doctor(&report, cli.json)
        }
        Command::Status => {
            let report = status::run_status(cli.device.as_deref())?;
            output::write_status(&report, cli.json)
        }
        Command::Connect => {
            let report = connect::run_connect(cli.device.as_deref())?;
            output::write_connect(&report, cli.json)
        }
        Command::Ls(args) => {
            if args.sort_size && args.sort_time {
                return Err(AppError::InvalidArguments {
                    message: "choose either --sort-size or --sort-time".to_string(),
                });
            }

            let report = ls::run_ls(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.take_over,
            )?;
            let sort = if args.sort_size {
                output::LsSort::Size
            } else if args.sort_time {
                output::LsSort::Time
            } else {
                output::LsSort::Name
            };
            output::write_ls(
                &report,
                cli.json,
                output::LsDisplayOptions {
                    long: args.long,
                    ids: args.ids,
                    size: args.size,
                    human_readable: args.human_readable,
                    sort,
                    reverse: args.reverse,
                },
            )
        }
        Command::Tree(args) => {
            let report = tree::run_tree(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.depth,
            )?;
            output::write_tree(&report, cli.json, args.ids)
        }
        Command::Stat(args) => {
            let report = stat::run_stat(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
            )?;
            output::write_stat(&report, cli.json)
        }
        Command::Pull(args) => {
            let report = pull::run_pull(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.local_path.as_deref(),
                pull::PullOptions {
                    recursive: args.recursive,
                    overwrite: args.overwrite,
                    skip_existing: args.skip_existing,
                    dry_run: args.dry_run,
                    progress: !cli.no_progress && !cli.json && !args.dry_run,
                },
            )?;
            output::write_pull(&report, cli.json)
        }
        Command::Push(args) => {
            let report = push::run_push(
                cli.device.as_deref(),
                cli.auto_connect,
                args.local_path.as_str(),
                args.remote_path.as_str(),
                push::PushOptions {
                    recursive: args.recursive,
                    overwrite: args.overwrite,
                    dry_run: args.dry_run,
                    progress: !cli.no_progress && !cli.json && !args.dry_run,
                },
            )?;
            output::write_push(&report, cli.json)
        }
        Command::Mkdir(args) => {
            let report = write_ops::run_mkdir(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.parents,
            )?;
            output::write_mkdir(&report, cli.json)
        }
        Command::Rm(args) => {
            let report = write_ops::run_rm(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.recursive,
                args.force,
                args.dry_run,
            )?;
            output::write_rm(&report, cli.json)
        }
        Command::Rename(args) => {
            let report = write_ops::run_rename(
                cli.device.as_deref(),
                cli.auto_connect,
                args.remote_path.as_str(),
                args.new_name.as_str(),
            )?;
            output::write_rename(&report, cli.json)
        }
        Command::Eject => {
            let report = eject::run_eject(cli.device.as_deref(), cli.auto_connect)?;
            output::write_eject(&report, cli.json)
        }
    }
}

fn init_logging(verbose: u8) {
    let default_filter = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug,nusb=debug",
    };

    let env = env_logger::Env::default().filter_or("TP7_LOG", default_filter);
    let _ = env_logger::Builder::from_env(env).try_init();
}
