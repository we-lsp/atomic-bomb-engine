use crate::models::api_endpoint::ApiEndpoint;
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::assert_option::AssertOption;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct AssertTask {
    pub(crate) assert_options: Vec<AssertOption>,
    pub(crate) body_bytes: Vec<u8>,
    pub(crate) verbose: bool,
    pub(crate) err_count: Arc<AtomicUsize>,
    pub(crate) api_err_count: Arc<Mutex<i32>>,
    pub(crate) assert_errors: Arc<Mutex<AssertErrorStats>>,
    pub(crate) endpoint: Arc<Mutex<ApiEndpoint>>,
    pub(crate) api_name: String,
    pub(crate) successful_requests: Arc<AtomicUsize>,
    pub(crate) api_successful_requests: Arc<AtomicUsize>,
    pub(crate) completion_signal: oneshot::Sender<()>,
}
