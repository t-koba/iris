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
