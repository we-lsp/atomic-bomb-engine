use reqwest;
use tokio;
use histogram::{Histogram, Error};
use std::time::{Instant, Duration};
use std::sync::{Arc, Mutex};
use tokio::time::interval;
use indicatif::ProgressBar;
use clap::Parser;

struct TestResult {
    total_duration: Duration,
    success_rate: f64,
    median_response_time: u64,
    response_time_95: u64,
    response_time_99: u64,
    total_requests: i32,
    rps: f64,
    max_response_time: u64,
    min_response_time: u64,
}

async fn run(url: &str, test_duration_secs: u64, concurrent_requests: i32) -> Result<TestResult, Error> {
    let histogram = Arc::new(Mutex::new(Histogram::new(4, 14).unwrap()));
    let successful_requests = Arc::new(Mutex::new(0));
    let total_requests = Arc::new(Mutex::new(0));
    let test_start = Instant::now();
    let test_end = test_start + Duration::from_secs(test_duration_secs);
    let max_response_time = Arc::new(Mutex::new(0u64));
    let min_response_time = Arc::new(Mutex::new(u64::MAX));

    let mut handles = Vec::new();

    for _ in 0..concurrent_requests {
        let client = reqwest::Client::new();
        let url = url.to_string();
        let histogram_clone = histogram.clone();
        let successful_requests_clone = successful_requests.clone();
        let total_requests_clone = total_requests.clone();
        let test_end = test_end;
        let max_response_time_clone = max_response_time.clone();
        let min_response_time_clone = min_response_time.clone();

        let handle = tokio::spawn(async move {
            while Instant::now() < test_end {
                *total_requests_clone.lock().unwrap() += 1;
                let start = Instant::now();
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        let duration = start.elapsed().as_millis() as u64;
                        let mut max_rt = max_response_time_clone.lock().unwrap();
                        let mut min_rt = min_response_time_clone.lock().unwrap();
                        *max_rt = (*max_rt).max(duration);
                        *min_rt = (*min_rt).min(duration);
                        match histogram_clone.lock().unwrap().increment(duration) {
                            Ok(_) => {},
                            Err(err) => eprintln!("错误:{}", err),
                        }
                        *successful_requests_clone.lock().unwrap() += 1;
                    },
                    _ => {}
                }
            }
        });

        handles.push(handle);
    }
    // 打印进度条
    let pb = ProgressBar::new(test_duration_secs);
    let progress_interval = Duration::from_secs(1); // 每1秒更新一次进度
    let mut interval = interval(progress_interval);
    let _ = tokio::spawn(async move {
        while Instant::now() < test_end {
            interval.tick().await;
            let elapsed = Instant::now().duration_since(test_start).as_secs();
            pb.inc(1);
        }
        pb.finish_with_message("done");
    });

    for handle in handles {
        handle.await.unwrap();
    }

    let total_duration = Duration::from_secs(test_duration_secs);
    let total_requests = *total_requests.lock().unwrap() as f64;
    let successful_requests = *successful_requests.lock().unwrap() as f64;
    let success_rate = successful_requests / total_requests * 100.0;
    let histogram = histogram.lock().unwrap();

    let test_result = TestResult {
        total_duration,
        success_rate,
        median_response_time: *histogram.percentile(50.0)?.range().start(),
        response_time_95: *histogram.percentile(95.0)?.range().start(),
        response_time_99: *histogram.percentile(99.0)?.range().start(),
        total_requests: total_requests as i32,
        rps: successful_requests / test_duration_secs as f64,
        max_response_time: *max_response_time.lock().unwrap(),
        min_response_time: *min_response_time.lock().unwrap(),
    };

    Ok(test_result)
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// 目标地址
    #[arg(short, long)]
    url: String,

    /// 持续时间
    #[arg(short, long, default_value_t = 1)]
    test_duration_secs: u64,

    /// 并发数
    #[arg(short, long, default_value_t = 1)]
    concurrent_requests: i32,
}
#[tokio::main]
async fn main() {
    let args = Args::parse();

    match run(&args.url, args.test_duration_secs, args.concurrent_requests).await {
        Ok(result) => {
            println!("测试完成");
            println!("持续时间: {:?}", result.total_duration);
            println!("RPS:{:?}", result.rps);
            println!("总请求数：{:?}", result.total_requests);
            println!("成功率: {:.2}%", result.success_rate);
            println!("最大响应时间:{:.2}ms", result.max_response_time);
            println!("最小响应时间:{:.2}ms", result.min_response_time);
            println!("中位响应时间: {} ms", result.median_response_time);
            println!("95%响应时间: {} ms", result.response_time_95);
            println!("99%响应时间: {} ms", result.response_time_99);
        },
        Err(e) => println!("Error: {}", e),
    }
}