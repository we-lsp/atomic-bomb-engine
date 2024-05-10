use std::collections::BTreeMap;
use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Error;
use futures::future::join_all;
use handlebars::Handlebars;
use histogram::Histogram;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use url::Url;

use crate::core::check_endpoints_names::check_endpoints_names;
use crate::core::concurrency_controller::ConcurrencyController;
use crate::core::sleep_guard::SleepGuard;
use crate::core::{listening_assert, setup, share_result, start_task};
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::http_error_stats::HttpErrorStats;
use crate::models::result::{ApiResult, BatchResult};
use crate::models::setup::SetupApiEndpoint;
use crate::models::step_option::{InnerStepOption, StepOption};

pub async fn batch(
    result_sender: mpsc::Sender<Option<BatchResult>>,
    test_duration_secs: u64,
    concurrent_requests: usize,
    timeout_secs: u64,
    cookie_store_enable: bool,
    verbose: bool,
    should_prevent: bool,
    api_endpoints: Vec<ApiEndpoint>,
    step_option: Option<StepOption>,
    setup_options: Option<Vec<SetupApiEndpoint>>,
    mut assert_channel_buffer_size: usize,
) -> anyhow::Result<BatchResult> {
    // 阻止电脑休眠
    let _guard = SleepGuard::new(should_prevent);
    // 检查阶梯并发量
    if let Some(step_option) = step_option.clone() {
        // 计算总共增加次数
        let total_steps = test_duration_secs / step_option.increase_interval;
        // 计算总增加并发数
        let total_concurrency_increase =
            step_option.increase_step as u64 * total_steps * (total_steps + 1) / 2;
        if total_concurrency_increase < concurrent_requests as u64 {
            return Err(Error::msg(
                "阶梯加压总并发数在设置的时间内无法增加到预设的结束并发数",
            ));
        }
    };
    // 检查每个接口的名称
    if let Err(e) = check_endpoints_names(api_endpoints.clone()) {
        return Err(Error::msg(e));
    }
    // 总响应时间统计
    let histogram = match Histogram::new(14, 20) {
        Ok(h) => Arc::new(Mutex::new(h)),
        Err(e) => {
            return Err(Error::msg(format!("获取存储桶失败::{:?}", e.to_string())));
        }
    };
    // 成功数据统计
    let successful_requests = Arc::new(AtomicUsize::new(0));
    // 请求总数统计
    let total_requests = Arc::new(AtomicUsize::new(0));
    // 统计最大响应时间
    let max_response_time = Arc::new(Mutex::new(0u64));
    // 统计最小响应时间
    let min_response_time = Arc::new(Mutex::new(u64::MAX));
    // 统计错误数量
    let err_count = Arc::new(AtomicUsize::new(0));
    // 统计每秒错误数
    let number_of_last_errors = Arc::new(AtomicUsize::new(0));
    // 已开始并发数
    let concurrent_number = Arc::new(AtomicUsize::new(0));
    // 接口线程池
    let mut handles: Vec<JoinHandle<Result<(), Error>>> = Vec::new();
    // 统计响应大小
    let total_response_size = Arc::new(AtomicUsize::new(0));
    // 统计http错误
    let http_errors = Arc::new(Mutex::new(HttpErrorStats::new()));
    // 统计断言错误
    let assert_errors = Arc::new(Mutex::new(AssertErrorStats::new()));
    // 总权重
    let total_weight: u32 = api_endpoints.iter().map(|e| e.weight).sum();
    // 是否停止通道
    let (should_stop_tx, should_stop_rx) = oneshot::channel();
    // 断言队列
    if assert_channel_buffer_size <= 0 {
        assert_channel_buffer_size = 1024
    }
    let (tx_assert, rx_assert) = mpsc::channel(assert_channel_buffer_size);
    // 开启一个任务，做断言的生产消费
    if api_endpoints
        .clone()
        .into_iter()
        .any(|item| item.assert_options.is_some())
    {
        if verbose {
            println!("开启断言消费任务");
        };
        tokio::spawn(listening_assert::listening_assert(rx_assert));
    };
    // 用arc包装每一个endpoint
    let api_endpoints_arc: Vec<Arc<Mutex<ApiEndpoint>>> = api_endpoints
        .into_iter()
        .map(|endpoint| Arc::new(Mutex::new(endpoint)))
        .collect();
    // 开始测试时间
    let test_start = Instant::now();
    // 测试结束时间
    let test_end = test_start + Duration::from_secs(test_duration_secs);
    // 每个接口的测试结果
    let results: Vec<ApiResult> = Vec::new();
    let results_arc = Arc::new(Mutex::new(results));
    // user_agent
    let info = os_info::get();
    let os_type = info.os_type();
    let os_version = info.version().to_string();
    let app_name = env!("CARGO_PKG_NAME");
    let app_version = env!("CARGO_PKG_VERSION");
    let user_agent_value =
        match format!("{} {} ({}; {})", app_name, app_version, os_type, os_version)
            .parse::<HeaderValue>()
        {
            Ok(v) => v,
            Err(e) => {
                return Err(Error::msg(format!(
                    "解析user agent失败::{:?}",
                    e.to_string()
                )));
            }
        };
    let mut is_need_render_template = false;
    // 全局提取字典
    let mut extract_map: BTreeMap<String, Value> = BTreeMap::new();
    // 创建http客户端
    let builder = Client::builder()
        .cookie_store(cookie_store_enable)
        .default_headers({
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, user_agent_value);
            headers
        });
    let client = match timeout_secs > 0 {
        true => match builder.timeout(Duration::from_secs(timeout_secs)).build() {
            Ok(cli) => cli,
            Err(e) => return Err(Error::msg(format!("构建含有超时的http客户端失败: {:?}", e))),
        },
        false => match builder.build() {
            Ok(cli) => cli,
            Err(e) => return Err(Error::msg(format!("构建http客户端失败: {:?}", e))),
        },
    };
    // 开始初始化
    if let Some(setup_options) = setup_options {
        is_need_render_template = true;
        match setup::start_setup(setup_options, extract_map.clone(), client.clone()).await {
            Ok(res) => {
                if let Some(extract) = res {
                    extract_map.extend(extract);
                };
            }
            Err(e) => return Err(Error::msg(format!("全局初始化失败: {:?}", e))),
        };
    };
    // println!("extract_map:{:?}", extract_map);
    // 并发安全的提取字典
    let extract_map_arc = Arc::new(Mutex::new(extract_map));
    // 针对每一个接口开始配置
    for (index, endpoint_arc) in api_endpoints_arc.clone().into_iter().enumerate() {
        let endpoint = endpoint_arc.lock().await;
        let weight = endpoint.weight.clone();
        let name = endpoint.name.clone();
        let api_url = match is_need_render_template {
            true => {
                // 使用模版替换cookies
                let handlebars = Handlebars::new();
                match handlebars.render_template(
                    &*endpoint.url.clone(),
                    &json!(*extract_map_arc.lock().await),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{:?}", e);
                        endpoint.url.clone()
                    }
                }
            }
            false => endpoint.url.clone(),
        };
        drop(endpoint);
        // 计算权重比例
        let weight_ratio = weight as f64 / total_weight as f64;
        // 计算每个接口的并发量
        let mut concurrency_for_endpoint =
            ((concurrent_requests as f64) * weight_ratio).round() as usize;
        // 如果这个接口的并发量四舍五入成0了， 就把他定为1
        if concurrency_for_endpoint == 0 {
            concurrency_for_endpoint = 1
        }
        // 接口数据的统计
        let api_histogram = match Histogram::new(14, 20) {
            Ok(h) => Arc::new(Mutex::new(h)),
            Err(e) => return Err(Error::msg(format!("获取存储桶失败::{:?}", e.to_string()))),
        };
        // 接口成功数据统计
        let api_successful_requests = Arc::new(AtomicUsize::new(0));
        // 接口请求总数统计
        let api_total_requests = Arc::new(AtomicUsize::new(0));
        // 接口统计最大响应时间
        let api_max_response_time = Arc::new(Mutex::new(0u64));
        // 接口统计最小响应时间
        let api_min_response_time = Arc::new(Mutex::new(u64::MAX));
        // 接口统计错误数量
        let api_err_count = Arc::new(AtomicUsize::new(0));
        // 接口并发数统计
        let api_concurrent_number = Arc::new(AtomicUsize::new(0));
        // 接口响应大小
        let api_total_response_size = Arc::new(AtomicUsize::new(0));
        // 初始化api结果
        let mut init_api_res = ApiResult::new();
        init_api_res.name = name.clone();
        init_api_res.url = api_url.clone();
        init_api_res.method = endpoint_arc.lock().await.method.clone().to_uppercase();
        // 包装初始化好的接口信息
        let api_result = Arc::new(Mutex::new(init_api_res.clone()));
        // 将初始化好的接口信息添加到list中
        results_arc.lock().await.push(init_api_res);
        // 根据step初始化并发控制器
        let controller = match step_option.clone() {
            None => Arc::new(ConcurrencyController::new(concurrency_for_endpoint, None)),
            Some(option) => {
                // 计算每个接口的步长
                let step = option.increase_step as f64 * weight_ratio;
                Arc::new(ConcurrencyController::new(
                    concurrency_for_endpoint,
                    Option::from(InnerStepOption {
                        increase_step: step,
                        increase_interval: option.increase_interval,
                    }),
                ))
            }
        };
        // 后台启动并发控制器
        tokio::spawn({
            let controller_clone = Arc::clone(&controller);
            async move {
                controller_clone.distribute_permits().await;
            }
        });
        // 将新url替换到每个接口中
        endpoint_arc.lock().await.url = api_url.clone();
        for _ in 0..concurrency_for_endpoint {
            // 开启并发
            let handle: JoinHandle<Result<(), Error>> =
                tokio::spawn(start_task::start_concurrency(
                    client.clone(),                       // http客户端
                    Arc::clone(&controller),              // 并发控制器
                    Arc::clone(&api_concurrent_number),   // api并发数
                    Arc::clone(&concurrent_number),       // 总并发数
                    Arc::clone(&extract_map_arc),         // 断言替换字典
                    Arc::clone(&endpoint_arc),            // 接口数据
                    Arc::clone(&total_requests),          // 总请求数
                    Arc::clone(&api_total_requests),      // api请求数
                    Arc::clone(&histogram),               // 总统计桶
                    Arc::clone(&api_histogram),           // api统计桶
                    Arc::clone(&max_response_time),       // 最大响应时间
                    Arc::clone(&api_max_response_time),   // 接口最大响应时间
                    Arc::clone(&min_response_time),       // 最小响应时间
                    Arc::clone(&api_min_response_time),   // api最小响应时间
                    Arc::clone(&total_response_size),     // 总响应数据
                    Arc::clone(&api_total_response_size), // api响应数据
                    Arc::clone(&api_err_count),           // api错误数
                    Arc::clone(&successful_requests),     // 成功数量
                    Arc::clone(&err_count),               // 错误数量
                    Arc::clone(&http_errors),             // http错误统计
                    Arc::clone(&assert_errors),           // 断言错误统计
                    Arc::clone(&api_successful_requests), // api成功数量
                    Arc::clone(&api_result),              // 接口详细统计
                    Arc::clone(&results_arc),             // 最终想起结果
                    tx_assert.clone(),                    // 断言通道
                    test_start,                           // 测试开始时间
                    test_end,                             // 测试结束时间
                    is_need_render_template,              // 是否需要读取模板
                    verbose,                              // 是否打印详情
                    index,                                // 索引
                ));
            handles.push(handle);
        }
        // println!("err count:{:?}",api_err_count.lock().await);
    }

    // 共享任务状态
    tokio::spawn(share_result::collect_results(
        result_sender,
        should_stop_rx,
        Arc::clone(&total_requests),
        Arc::clone(&successful_requests),
        Arc::clone(&histogram),
        Arc::clone(&total_response_size),
        Arc::clone(&http_errors),
        Arc::clone(&err_count),
        Arc::clone(&max_response_time),
        Arc::clone(&min_response_time),
        Arc::clone(&assert_errors),
        Arc::clone(&results_arc),
        Arc::clone(&concurrent_number),
        Arc::clone(&number_of_last_errors),
        verbose,
        test_start,
    ));

    // 等待任务完成
    let task_results = join_all(handles).await;
    for task_result in task_results {
        match task_result {
            Ok(res) => {
                match res {
                    Ok(_) => {
                        if verbose {
                            println!("任务完成")
                        }
                    }
                    Err(e) => {
                        eprintln!("异步任务内部错误::{:?}", e)
                    }
                };
            }
            Err(err) => {
                eprintln!("协程被取消或意外停止::{:?}", err);
            }
        };
    }

    // 对结果进行赋值
    let err_count_clone = Arc::clone(&err_count);
    let err_count = err_count_clone.load(Ordering::SeqCst);
    let total_duration = (Instant::now() - test_start).as_secs_f64();
    let total_requests = total_requests.load(Ordering::SeqCst) as u64;
    let successful_requests = successful_requests.load(Ordering::SeqCst) as f64;
    let success_rate = successful_requests / total_requests as f64 * 100.0;
    let histogram = histogram.lock().await;
    let total_response_size_kb = total_response_size.load(Ordering::SeqCst) as f64 / 1024.0;
    let throughput_kb_s = total_response_size_kb / test_duration_secs as f64;
    let http_errors = http_errors.lock().await.errors.clone();
    let assert_errors = assert_errors.lock().await.errors.clone();
    let timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(n) => n.as_millis(),
        Err(_) => 0,
    };
    let mut api_results = results_arc.lock().await;
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
    let error_rate = err_count as f64 / total_requests as f64 * 100.0;
    let total_concurrent_number_clone = concurrent_number.load(Ordering::SeqCst) as i32;
    // 总错误数量减去上一次错误数量得出增量
    let errors_per_second = err_count - number_of_last_errors.load(Ordering::SeqCst);
    // 将增量累加到上一次错误数量
    number_of_last_errors.fetch_add(errors_per_second, Ordering::Relaxed);
    // 最终结果
    let result = Ok(BatchResult {
        total_duration,
        success_rate,
        error_rate,
        median_response_time: match histogram.percentile(50.0) {
            Ok(b) => *b.range().start(),
            Err(e) => {
                return Err(Error::msg(format!("获取50线失败::{:?}", e.to_string())));
            }
        },
        response_time_95: match histogram.percentile(95.0) {
            Ok(b) => *b.range().start(),
            Err(e) => {
                return Err(Error::msg(format!("获取95线失败::{:?}", e.to_string())));
            }
        },
        response_time_99: match histogram.percentile(99.0) {
            Ok(b) => *b.range().start(),
            Err(e) => {
                return Err(Error::msg(format!("获取99线失败::{:?}", e.to_string())));
            }
        },
        total_requests,
        rps: total_requests as f64 / total_duration,
        max_response_time: *max_response_time.lock().await,
        min_response_time: *min_response_time.lock().await,
        err_count: err_count_clone.load(Ordering::SeqCst) as i32,
        total_data_kb: total_response_size_kb,
        throughput_per_second_kb: throughput_kb_s,
        http_errors: http_errors.lock().await.clone(),
        timestamp,
        assert_errors: assert_errors.lock().await.clone(),
        total_concurrent_number: total_concurrent_number_clone,
        api_results: api_results.to_vec().clone(),
        errors_per_second,
    });
    should_stop_tx.send(()).unwrap();
    eprintln!("测试完成！");
    result
}
