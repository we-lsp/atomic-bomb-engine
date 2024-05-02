use crate::models::assert_error_stats::AssertErrKey;
use crate::models::http_error_stats::HttpErrKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub total_duration: f64,
    pub success_rate: f64,
    pub median_response_time: u64,
    pub response_time_95: u64,
    pub response_time_99: u64,
    pub total_requests: i32,
    pub rps: f64,
    pub max_response_time: u64,
    pub min_response_time: u64,
    pub err_count: i32,
    pub total_data_kb: f64,
    pub throughput_per_second_kb: f64,
    pub http_errors: HashMap<HttpErrKey, u32>,
    pub timestamp: u128,
    pub assert_errors: HashMap<(String, String), u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    // 运行时间
    pub total_duration: f64,
    // 成功率
    pub success_rate: f64,
    // 错误率
    pub error_rate: f64,
    // 中位响应时间
    pub median_response_time: u64,
    // 95位响应时间
    pub response_time_95: u64,
    // 99位响应时间
    pub response_time_99: u64,
    // 总请求数
    pub total_requests: u64,
    // rps
    pub rps: f64,
    // 最大响应时间
    pub max_response_time: u64,
    // 最小响应时间
    pub min_response_time: u64,
    // 错误数量
    pub err_count: i32,
    // 总响应数据量(KB)
    pub total_data_kb: f64,
    // 每秒响应数据量
    pub throughput_per_second_kb: f64,
    // http错误详情
    pub http_errors: HashMap<HttpErrKey, u32>,
    // 时间戳
    pub timestamp: u128,
    // 断言错误详情
    pub assert_errors: HashMap<AssertErrKey, u32>,
    // 总并发数
    pub total_concurrent_number: i32,
    // api响应详情
    pub api_results: Vec<ApiResult>,
    // 每秒错误数
    pub errors_per_second: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResult {
    pub name: String,
    pub url: String,
    pub host: String,
    pub path: String,
    pub method: String,
    pub success_rate: f64,
    pub error_rate: f64,
    pub median_response_time: u64,
    pub response_time_95: u64,
    pub response_time_99: u64,
    pub total_requests: u64,
    pub rps: f64,
    pub max_response_time: u64,
    pub min_response_time: u64,
    pub err_count: i32,
    pub total_data_kb: f64,
    pub throughput_per_second_kb: f64,
    pub concurrent_number: i32,
}

impl ApiResult {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            host: String::new(),
            path: String::new(),
            method: String::new(),
            success_rate: 0.0,
            error_rate: 0.0,
            median_response_time: 0,
            response_time_95: 0,
            response_time_99: 0,
            total_requests: 0,
            rps: 0.0,
            max_response_time: 0,
            min_response_time: 0,
            err_count: 0,
            total_data_kb: 0.0,
            throughput_per_second_kb: 0.0,
            concurrent_number: 0,
        }
    }
}
