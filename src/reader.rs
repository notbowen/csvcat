use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

pub struct CsvData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub fn load_csv(path: &Path, delimiter: u8) -> anyhow::Result<CsvData> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    // Hint to the OS: we'll scan the file front-to-back
    #[cfg(unix)]
    mmap.advise(memmap2::Advice::Sequential)?;

    let mut reader = csv_parser::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_reader(mmap.as_ref());

    let headers: Vec<String> = reader
        .headers()?
        .iter()
        .map(str::to_string)
        .collect();

    // Reuse a single StringRecord allocation for all rows
    let mut record = csv_parser::StringRecord::new();
    let mut rows: Vec<Vec<String>> = Vec::new();

    while reader.read_record(&mut record)? {
        rows.push(record.iter().map(str::to_string).collect());
    }

    Ok(CsvData { headers, rows })
}
