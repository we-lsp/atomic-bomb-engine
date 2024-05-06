use crate::models::multipart_option::MultipartOption;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct JsonpathExtract {
    pub key: String,
    pub jsonpath: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SetupApiEndpoint {
    pub name: String,
    pub url: String,
    pub method: String,
    pub json: Option<Value>,
    pub form_data: Option<HashMap<String, String>>,
    pub multipart_options: Option<Vec<MultipartOption>>,
    pub headers: Option<HashMap<String, String>>,
    pub cookies: Option<String>,
    pub jsonpath_extract: Option<Vec<JsonpathExtract>>,
}
