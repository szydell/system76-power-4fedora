// Copyright 2018-2022 System76 <info@system76.com>
//
// SPDX-License-Identifier: GPL-3.0-only

use clap::{builder::PossibleValuesParser, Parser};

#[derive(Parser)]
#[clap(
    about = "Query or set the graphics mode",
    long_about = "Query or set the graphics mode.\n\n - If an argument is not provided, the \
                  graphics profile will be queried\n - Otherwise, that profile will be set, if it \
                  is a valid profile\n\nA reboot is required after switching modes."
)]
pub enum GraphicsArgs {
    #[clap(about = "Like integrated, but the dGPU is available for compute")]
    Compute,
    #[clap(about = "Set the graphics mode to Hybrid (PRIME)")]
    Hybrid,
    #[clap(about = "Set the graphics mode to integrated")]
    Integrated,
    #[clap(about = "Set the graphics mode to NVIDIA")]
    Nvidia,
    #[clap(about = "Determines if the system has switchable graphics")]
    Switchable,
    #[clap(about = "Query or set the discrete graphics power state")]
    Power {
        #[clap(help = "Set whether discrete graphics should be on or off")]
        #[arg(
            value_parser = PossibleValuesParser::new(["auto", "off", "on"])
        )]
        state: Option<String>,
    },
}

#[derive(Parser)]
#[clap(
    name = "system76-power",
    about = "Utility for managing graphics and power profiles",
    version = env!("CARGO_PKG_VERSION"),
    subcommand_required = true,
    arg_required_else_help = true,
)]
pub enum Args {
    #[clap(
        about = "Runs the program in daemon mode",
        long_about = "Registers a new DBUS service and starts an event loop to listen for, and \
                      respond to, DBUS events from clients"
    )]
    Daemon {
        #[clap(
            short = 'q',
            long = "quiet",
            help = "Set the verbosity of daemon logs to 'off' [default is 'info']",
            global = true,
            group = "verbosity"
        )]
        quiet:   bool,
        #[clap(
            short = 'v',
            long = "verbose",
            help = "Set the verbosity of daemon logs to 'debug' [default is 'info']",
            global = true,
            group = "verbosity"
        )]
        verbose: bool,
    },
    #[clap(
        about = "Query or set the power profile",
        long_about = "Queries or sets the power profile.\n\n - If an argument is not provided, \
                      the power profile will be queried\n - Otherwise, that profile will be set, \
                      if it is a valid profile"
    )]
    Profile {
        #[clap(
            help = "set the power profile",
            default_value = None,
            value_parser = PossibleValuesParser::new(["battery", "balanced", "performance"]),
        )]
        profile: Option<String>,
    },
    Graphics {
        #[clap(subcommand)]
        cmd: Option<GraphicsArgs>,
    },
    #[clap(
        about = "Set thresholds for battery charging",
        // Autogenerated usage seemed to have issues
        override_usage = "system76-power charge-thresholds [<start> <end> | --profile <profile>]",
    )]
    ChargeThresholds {
        #[clap(
            long = "profile",
            help = "Profile name",
            value_parser = PossibleValuesParser::new(["full_charge", "balanced", "max_lifespan"]),
            group = "profile-or-thresholds",
        )]
        profile:       Option<String>,
        #[clap(long = "list-profiles", help = "List profiles", group = "profile-or-thresholds")]
        list_profiles: bool,
        #[clap(
            help = "Charge thresholds",
            value_parser = clap::value_parser!(u16).range(0..=100),
            number_of_values = 2,
            value_names = &["start", "end"],
            group = "profile-or-thresholds",
        )]
        thresholds:    Vec<String>,
    },
}
