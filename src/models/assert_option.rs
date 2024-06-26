use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AssertOption {
    pub jsonpath: String,
    pub reference_object: Value,
}
