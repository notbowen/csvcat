use clap::Parser;
use std::path::PathBuf;

mod reader;
mod table;
mod viewer;

/// A beautiful, fast CSV viewer — like batcat for CSV files
#[derive(Parser)]
#[command(name = "csvcat", version)]
struct Cli {
    /// CSV file to view
    file: PathBuf,

    /// Print directly to stdout without the interactive viewer
    #[arg(short, long)]
    print: bool,

    /// Field delimiter character
    #[arg(short, long, default_value = ",")]
    delimiter: char,

    /// Maximum column width (truncates longer values)
    #[arg(long, default_value = "40")]
    max_col_width: usize,

    /// Disable alternating row background colors
    #[arg(long = "no-zebra")]
    no_zebra: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data = reader::load_csv(&cli.file, cli.delimiter as u8)?;
    let zebra = !cli.no_zebra;

    if cli.print {
        viewer::print_table(&data, cli.max_col_width, zebra)
    } else {
        viewer::run_viewer(&data, cli.max_col_width, zebra)
    }
}
