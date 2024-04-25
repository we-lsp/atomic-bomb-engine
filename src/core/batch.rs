use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Error;
use futures::future::join_all;
use handlebars::Handlebars;
use histogram::Histogram;
use reqwest::header::{HeaderMap, USER_AGENT};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use crate::core::check_endpoints_names::check_endpoints_names;
use crate::core::concurrency_controller::ConcurrencyController;
use crate::core::sleep_guard::SleepGuard;
use crate::core::status_share::RESULTS_SHOULD_STOP;
use crate::core::{listening_assert, setup, share_result, start_task};
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::http_error_stats::HttpErrorStats;
use crate::models::result::{ApiResult, BatchResult};
use crate::models::setup::SetupApiEndpoint;
use crate::models::step_option::{InnerStepOption, StepOption};

pub async fn batch(
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
    // 检查每个接口的名称
    if let Err(e) = check_endpoints_names(api_endpoints.clone()) {
        return Err(Error::msg(e));
    }
    // 总响应时间统计
    let histogram = Arc::new(Mutex::new(Histogram::new(14, 20).unwrap()));
    // 成功数据统计
    let successful_requests = Arc::new(Mutex::new(0));
    // 请求总数统计
    let total_requests = Arc::new(Mutex::new(0));
    // 统计最大响应时间
    let max_response_time = Arc::new(Mutex::new(0u64));
    // 统计最小响应时间
    let min_response_time = Arc::new(Mutex::new(u64::MAX));
    // 统计错误数量
    let err_count = Arc::new(Mutex::new(0));
    // 已开始并发数
    let concurrent_number = Arc::new(Mutex::new(0));
    // 接口线程池
    let mut handles: Vec<JoinHandle<Result<(), Error>>> = Vec::new();
    // 统计响应大小
    let total_response_size = Arc::new(Mutex::new(0u64));
    // 统计http错误
    let http_errors = Arc::new(Mutex::new(HttpErrorStats::new()));
    // 统计断言错误
    let assert_errors = Arc::new(Mutex::new(AssertErrorStats::new()));
    // 总权重
    let total_weight: u32 = api_endpoints.iter().map(|e| e.weight).sum();
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
    let user_agent_value = format!("{} {} ({}; {})", app_name, app_version, os_type, os_version);
    let mut is_need_render_template = false;
    // 全局提取字典
    let mut extract_map: BTreeMap<String, Value> = BTreeMap::new();
    // 创建http客户端
    let builder = Client::builder()
        .cookie_store(cookie_store_enable)
        .default_headers({
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, user_agent_value.parse()?);
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
        results_arc.lock().await.push(ApiResult::new());
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
        let api_histogram = Arc::new(Mutex::new(Histogram::new(14, 20).unwrap()));
        // 接口成功数据统计
        let api_successful_requests = Arc::new(Mutex::new(0));
        // 接口请求总数统计
        let api_total_requests = Arc::new(Mutex::new(0));
        // 接口统计最大响应时间
        let api_max_response_time = Arc::new(Mutex::new(0u64));
        // 接口统计最小响应时间
        let api_min_response_time = Arc::new(Mutex::new(u64::MAX));
        // 接口统计错误数量
        let api_err_count = Arc::new(Mutex::new(0));
        // 接口并发数统计
        let api_concurrent_number = Arc::new(Mutex::new(0));
        // 接口响应大小
        let api_total_response_size = Arc::new(Mutex::new(0u64));
        // 初始化api结果
        let api_result = Arc::new(Mutex::new({
            let mut api_result = ApiResult::new();
            api_result.name = name.clone();
            api_result.url = api_url.clone();
            api_result
        }));
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
    let err_count = *err_count_clone.lock().await;
    let total_duration = (Instant::now() - test_start).as_secs_f64();
    let total_requests = *total_requests.lock().await as u64;
    let successful_requests = *successful_requests.lock().await as f64;
    let success_rate = successful_requests / total_requests as f64 * 100.0;
    let histogram = histogram.lock().await;
    let total_response_size_kb = *total_response_size.lock().await as f64 / 1024.0;
    let throughput_kb_s = total_response_size_kb / test_duration_secs as f64;
    let http_errors = http_errors.lock().await.errors.clone();
    let assert_errors = assert_errors.lock().await.errors.clone();
    let timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(n) => n.as_millis(),
        Err(_) => 0,
    };
    let api_results = results_arc.lock().await;
    let error_rate = err_count as f64 / total_requests as f64 * 100.0;
    let total_concurrent_number_clone = concurrent_number.lock().await.clone();
    // 最终结果
    let result = Ok(BatchResult {
        total_duration,
        success_rate,
        error_rate,
        median_response_time: *histogram.percentile(50.0)?.range().start(),
        response_time_95: *histogram.percentile(95.0)?.range().start(),
        response_time_99: *histogram.percentile(99.0)?.range().start(),
        total_requests,
        rps: total_requests as f64 / test_duration_secs as f64,
        max_response_time: *max_response_time.lock().await,
        min_response_time: *min_response_time.lock().await,
        err_count: *err_count_clone.lock().await,
        total_data_kb: total_response_size_kb,
        throughput_per_second_kb: throughput_kb_s,
        http_errors: http_errors.lock().await.clone(),
        timestamp,
        assert_errors: assert_errors.lock().await.clone(),
        total_concurrent_number: total_concurrent_number_clone,
        api_results: api_results.to_vec().clone(),
    });
    let mut should_stop = RESULTS_SHOULD_STOP.lock().await;
    *should_stop = true;
    eprintln!("测试完成！");
    result
}

/*
    单测
*/

#[cfg(test)]
mod tests {
    use core::option::Option;

    use crate::models::assert_option::AssertOption;
    use crate::models::setup::JsonpathExtract;

    use super::*;

    #[tokio::test]
    async fn test_batch() {
        let mut assert_vec: Vec<AssertOption> = Vec::new();
        let ref_obj = Value::from(2000000);
        assert_vec.push(AssertOption {
            jsonpath: "$.code".to_string(),
            reference_object: ref_obj,
        });
        let mut endpoints: Vec<ApiEndpoint> = Vec::new();

        endpoints.push(ApiEndpoint {
            name: "有断言".to_string(),
            url: "https://ooooo.run/api/short/v1/getJumpCount/{{test-code}}".to_string(),
            method: "GET".to_string(),
            weight: 1,
            json: None,
            form_data: None,
            headers: None,
            cookies: None,
            assert_options: Some(assert_vec.clone()),
            // think_time_option: Some(ThinkTime {
            //     min_millis: 300,
            //     max_millis: 500,
            // }),
            think_time_option: None,
            setup_options: None,
        });
        // //
        endpoints.push(ApiEndpoint {
            name: "无断言".to_string(),
            url: "https://ooooo.run/api/short/v1/getJumpCount".to_string(),
            method: "POST".to_string(),
            weight: 3,
            json: None,
            form_data: None,
            headers: None,
            cookies: None,
            assert_options: None,
            think_time_option: None,
            setup_options: None,
        });

        // endpoints.push(ApiEndpoint{
        //     name: "test-1".to_string(),
        //     url: "http://127.0.0.1:8080/".to_string(),
        //     method: "POST".to_string(),
        //     timeout_secs: 10,
        //     weight: 1,
        //     json: Some(json!({"name": "test","number": 10086})),
        //     headers: None,
        //     cookies: None,
        //     form_data:None,
        //     assert_options: None,
        // });
        let mut jsonpath_extracts: Vec<JsonpathExtract> = Vec::new();
        jsonpath_extracts.push(JsonpathExtract {
            key: "test-code".to_string(),
            jsonpath: "$.code".to_string(),
        });
        jsonpath_extracts.push(JsonpathExtract {
            key: "test-msg".to_string(),
            jsonpath: "$.msg".to_string(),
        });
        let mut setup: Vec<SetupApiEndpoint> = Vec::new();
        setup.push(SetupApiEndpoint {
            name: "初始化-1".to_string(),
            url: "https://ooooo.run/api/short/v1/list".to_string(),
            method: "get".to_string(),
            json: None,
            form_data: None,
            headers: None,
            cookies: None,
            jsonpath_extract: Some(jsonpath_extracts),
        });
        match batch(
            5,
            100,
            10,
            true,
            true,
            true,
            endpoints,
            Option::from(StepOption {
                increase_step: 5,
                increase_interval: 2,
            }),
            Option::from(setup),
            4096,
        )
        .await
        {
            Ok(r) => {
                println!("{:#?}", r)
            }
            Err(e) => {
                eprintln!("{:?}", e)
            }
        };
    }
}
