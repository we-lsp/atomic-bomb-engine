use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MultipartOption {
    pub form_key: String,
    pub path: String,
    pub file_name: String,
    pub mime: String,
}
