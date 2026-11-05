#[derive(Clone, Debug)]
pub struct ByteRange {
    pub offset: u64,
    /// `None` means up to the end.
    pub length: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ProgressInfo {
    pub bytes_completed: u64,
    pub bytes_total: Option<u64>,
    pub files_completed: u64,
    pub files_total: Option<u64>,
}

impl ProgressInfo {
    /// Progress of a one-file transfer; the file counts as transferred once `done`.
    pub fn single_file(bytes_completed: u64, bytes_total: Option<u64>, done: bool) -> Self {
        Self {
            bytes_completed,
            bytes_total,
            files_completed: u64::from(done),
            files_total: Some(1),
        }
    }
}
