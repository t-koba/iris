use super::object::ObjectRef;

#[derive(Clone, Debug)]
pub struct MultipartUploadSession {
    pub upload_id: String,
    pub destination: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultipartPart {
    pub part_number: i32,
    pub etag: String,
    pub size: u64,
}
