use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::http_error_stats::HttpErrorStats;
use crate::models::result::{ApiResult, BatchResult};
use histogram::Histogram;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Receiver;
use tokio::sync::Mutex;
use tokio::time::interval;
use url::Url;

pub(crate) async fn collect_results(
    result_channel: Sender<Option<BatchResult>>,
    should_stop_rx: Receiver<()>,
    total_requests: Arc<AtomicUsize>,
    successful_requests: Arc<AtomicUsize>,
    histogram: Arc<Mutex<Histogram>>,
    total_response_size: Arc<AtomicUsize>,
    http_errors: Arc<Mutex<HttpErrorStats>>,
    err_count: Arc<AtomicUsize>,
    max_resp_time: Arc<Mutex<u64>>,
    min_resp_time: Arc<Mutex<u64>>,
    assert_error: Arc<Mutex<AssertErrorStats>>,
    api_results: Arc<Mutex<Vec<ApiResult>>>,
    concurrent_number: Arc<AtomicUsize>,
    dura: Arc<Mutex<f64>>,
    number_of_last_requests: Arc<AtomicUsize>,
    number_of_last_errors: Arc<AtomicUsize>,
    verbose: bool,
    test_start: Instant,
) {
    let mut interval = interval(Duration::from_secs(1));
    select! {
        // 收到停止信号
        _ = should_stop_rx => {
            println!("收到停止信号");
            return;
        }
        // 定时收集结果
         _ = async {
            loop{
                interval.tick().await;
                let err_count = err_count.load(Ordering::SeqCst) as i32;
                let max_response_time_c = *max_resp_time.lock().await;
                let min_response_time_c = *min_resp_time.lock().await;
                let total_duration = (Instant::now() - test_start).as_secs_f64();
                let mut d = dura.lock().await;
                let this_duration = total_duration - *d;
                *d = total_duration;
                let total_requests = total_requests.load(Ordering::SeqCst) as f64;
                let successful_requests = successful_requests.load(Ordering::SeqCst) as f64;
                let success_rate = match total_requests == 0f64 {
                true => 0f64,
                false => successful_requests / total_requests * 100.0,
                };
                let error_rate = match total_requests == 0f64 {
                true => 0f64,
                false => err_count as f64 / total_requests * 100.0,
                };
                let histogram = histogram.lock().await;
                let total_response_size_kb = total_response_size.load(Ordering::SeqCst) as f64 / 1024.0;
                let throughput_kb_s = total_response_size_kb / total_duration;
                let http_errors = http_errors.lock().await.errors.clone();
                let assert_errors = assert_error.lock().await.errors.clone();
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
                let mut api_results = api_results.lock().await;
                // 计算每个接口的rps,host, path
                for (index, res) in api_results.clone().into_iter().enumerate() {
                // 计算每个接口的rps
                let rps = res.total_requests as f64 / total_duration;
                api_results[index].rps = rps;
                // 计算每个接口的HOST，PATH
                if let Ok(url) = Url::parse(&*res.url) {
                if let Some(host) = url.host() {
                api_results[index].host = host.to_string();
                };
                api_results[index].path = url.path().to_string();
                };
                }
                let total_concurrent_number = concurrent_number.load(Ordering::SeqCst) as i32;
                // 总错误数量减去上一次错误数量得出增量
                let errors_per_second = err_count as usize - number_of_last_errors.load(Ordering::SeqCst);
                // 将增量累加到上一次错误数量
                number_of_last_errors.fetch_add(errors_per_second, Ordering::Relaxed);
                // 将请求数量减去上一次请求数量得出增量
                let requests_per_second = total_requests as usize - number_of_last_requests.load(Ordering::SeqCst);
                // 将增量累加
                number_of_last_requests.fetch_add(requests_per_second, Ordering::Relaxed);
                // 共享队列
                let result = BatchResult {
                total_duration,
                success_rate,
                error_rate,
                median_response_time: resp_median_line,
                response_time_95: resp_95_line,
                response_time_99: resp_99_line,
                total_requests: total_requests as u64,
                rps: requests_per_second as f64 / this_duration,
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
                errors_per_second,
                };
                let elapsed = test_start.elapsed();
                if verbose {
                println!("{:?}-{:#?}", elapsed.as_millis(), result.clone());
                };
                let _ = result_channel.send(Some(result)).await;
            }
        } => {
            eprintln!("推送意外停止")
        }
    }
}
