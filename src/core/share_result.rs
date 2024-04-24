use crate::core::status_share::{RESULTS_QUEUE, RESULTS_SHOULD_STOP};
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::http_error_stats::HttpErrorStats;
use crate::models::result::{ApiResult, BatchResult};
use histogram::Histogram;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::interval;

pub(crate) async fn collect_results(
    total_requests: Arc<Mutex<usize>>,
    successful_requests: Arc<Mutex<i32>>,
    histogram: Arc<Mutex<Histogram>>,
    total_response_size: Arc<Mutex<u64>>,
    http_errors: Arc<Mutex<HttpErrorStats>>,
    err_count: Arc<Mutex<i32>>,
    max_resp_time: Arc<Mutex<u64>>,
    min_resp_time: Arc<Mutex<u64>>,
    assert_error: Arc<Mutex<AssertErrorStats>>,
    api_results: Arc<Mutex<Vec<ApiResult>>>,
    concurrent_number: Arc<Mutex<i32>>,
    verbose: bool,
    test_start: Instant,
) {
    let mut interval = interval(Duration::from_secs(1));
    let should_stop = *RESULTS_SHOULD_STOP.lock().await;
    while !should_stop {
        interval.tick().await;

        let err_count = *err_count.lock().await;
        let max_response_time_c = *max_resp_time.lock().await;
        let min_response_time_c = *min_resp_time.lock().await;
        let total_duration = (Instant::now() - test_start).as_secs_f64();
        let total_requests = *total_requests.lock().await as f64;
        let successful_requests = *successful_requests.lock().await as f64;
        let success_rate = successful_requests / total_requests * 100.0;
        let error_rate = err_count as f64 / total_requests * 100.0;
        let histogram = histogram.lock().await;
        let total_response_size_kb = *total_response_size.lock().await as f64 / 1024.0;
        let throughput_kb_s = total_response_size_kb / total_duration;
        let http_errors = http_errors.lock().await.errors.clone();
        let assert_errors = assert_error.lock().await.errors.clone();
        let rps = total_requests / total_duration;
        let resp_median_line = match histogram.percentile(50.0) {
            Ok(bucket) => *bucket.range().start(),
            Err(_) => 0,
        };
        let resp_95_line = match histogram.percentile(95.0) {
            Ok(bucket) => *bucket.range().start(),
            Err(_) => 0,
        };
        let resp_99_line = match histogram.percentile(99.0) {
            Ok(bucket) => *bucket.range().start(),
            Err(_) => 0,
        };
        let timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(n) => n.as_millis(),
            Err(_) => 0,
        };
        let api_results = api_results.lock().await;
        let total_concurrent_number = *concurrent_number.lock().await;
        let mut queue = RESULTS_QUEUE.lock().await;
        if queue.len() == 1 {
            queue.pop_front();
        }
        let result = BatchResult {
            total_duration,
            success_rate,
            error_rate,
            median_response_time: resp_median_line,
            response_time_95: resp_95_line,
            response_time_99: resp_99_line,
            total_requests: total_requests as u64,
            rps,
            max_response_time: max_response_time_c,
            min_response_time: min_response_time_c,
            err_count,
            total_data_kb: total_response_size_kb,
            throughput_per_second_kb: throughput_kb_s,
            http_errors: http_errors.lock().await.clone(),
            timestamp,
            assert_errors: assert_errors.lock().await.clone(),
            total_concurrent_number,
            api_results: api_results.to_vec().clone(),
        };
        let elapsed = test_start.elapsed();
        if verbose {
            println!("{:?}-{:#?}", elapsed.as_millis(), result.clone());
        };
        queue.push_back(result);
    }
}
