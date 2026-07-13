mod artifacts;
mod cli;
mod dist;
mod initramfs;
mod product;
mod product_contract;
mod qemu;
mod test;
mod workflow;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    workflow::dispatch(cli::Cli::parse())
}
