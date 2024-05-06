use crate::models::assert_option::AssertOption;
use crate::models::multipart_option::MultipartOption;
use crate::models::setup::SetupApiEndpoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ThinkTime {
    pub min_millis: u64,
    pub max_millis: u64,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ApiEndpoint {
    pub name: String,
    pub url: String,
    pub method: String,
    pub weight: u32,
    pub json: Option<Value>,
    pub form_data: Option<HashMap<String, String>>,
    pub multipart_options: Option<Vec<MultipartOption>>,
    pub headers: Option<HashMap<String, String>>,
    pub cookies: Option<String>,
    pub assert_options: Option<Vec<AssertOption>>,
    pub think_time_option: Option<ThinkTime>,
    pub setup_options: Option<Vec<SetupApiEndpoint>>,
}
