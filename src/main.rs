#![allow(non_snake_case)]
use clap::AppSettings::DeriveDisplayOrder;
use clap::Parser;
use log::{info, warn};
use std::path::PathBuf;

pub mod call;
pub mod consensus;
pub mod motif;
pub mod parse_bam;
pub mod phase_insertions;
pub mod repeats;
pub mod utils;
pub mod vcf;

// The arguments end up in the Cli struct
#[derive(Parser, Debug)]
#[structopt(global_settings=&[DeriveDisplayOrder])]
#[clap(author, version, about="Tool to genotype STRs from long reads", long_about = None)]
pub struct Cli {
    /// reference genome
    #[clap(validator=is_file)]
    fasta: String,

    /// bam file to call STRs in
    #[clap(validator=is_file)]
    bam: String,

    /// region string to genotype expansion in
    #[clap(short, long, value_parser)]
    region: Option<String>,

    /// Bed file with region(s) to genotype expansion(s) in
    #[clap(short = 'R', long, value_parser, validator=is_file)]
    region_file: Option<String>,

    /// minimal length of insertion/deletion operation
    #[clap(short, long, value_parser, default_value_t = 5)]
    minlen: usize,

    /// minimal number of supporting reads per haplotype
    #[clap(short, long, value_parser, default_value_t = 3)]
    support: usize,

    /// Number of parallel threads to use
    #[clap(short, long, value_parser, default_value_t = 8)]
    threads: usize,

    /// Sample name to use in VCF header, if not provided, the bam file name is used
    #[clap(long, value_parser)]
    sample: Option<String>,

    /// Print information on somatic variability
    #[clap(long, value_parser, default_value_t = false)]
    somatic: bool,

    /// Reads are not phased
    #[clap(long, value_parser, default_value_t = false)]
    unphased: bool,

    /// Identify poorly supported outlier expansions (only with --unphased)
    #[clap(long, value_parser, default_value_t = false)]
    find_outliers: bool,
}

fn is_file(pathname: &str) -> Result<(), String> {
    let path = PathBuf::from(pathname);
    if path.is_file() || pathname.starts_with("http") {
        Ok(())
    } else {
        Err(format!("Input file {} is invalid", path.display()))
    }
}

fn main() {
    env_logger::init();
    let args = Cli::parse();
    if args.find_outliers && !args.unphased {
        warn!("--find-outliers is only effective with --unphased");
    }
    info!("Collected arguments");
    call::genotype_repeats(args);
}

#[cfg(test)]
#[ctor::ctor]
fn init() {
    env_logger::init();
}

#[test]
fn verify_app() {
    use clap::CommandFactory;
    Cli::command().debug_assert()
}
