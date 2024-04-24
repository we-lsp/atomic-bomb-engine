use std::collections::BTreeMap;
use std::env;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Error;
use futures::future::join_all;
use futures::stream::StreamExt;
use handlebars::Handlebars;
use histogram::Histogram;
use rand::rngs::StdRng;
use rand::Rng;
use rand::SeedableRng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, USER_AGENT};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::core::check_endpoints_names::check_endpoints_names;
use crate::core::concurrency_controller::ConcurrencyController;
use crate::core::sleep_guard::SleepGuard;
use crate::core::status_share::RESULTS_SHOULD_STOP;
use crate::core::{listening_assert, setup, share_result};
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::assert_task::AssertTask;
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
        let url = endpoint.url.clone();
        let api_url = match is_need_render_template {
            true => {
                // 使用模版替换cookies
                let handlebars = Handlebars::new();
                match handlebars.render_template(&*url, &json!(*extract_map_arc.lock().await)) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{:?}", e);
                        url.clone()
                    }
                }
            }
            false => url.clone(),
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
        let mut r = ApiResult::new();
        r.name = name.clone();
        r.url = api_url.clone();
        let api_result = Arc::new(Mutex::new(r));
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
        for _ in 0..concurrency_for_endpoint {
            // 数据桶副本
            let histogram_clone = histogram.clone();
            // 任务名称
            let api_name_clone = name.clone();
            // url副本
            let api_url_clone = api_url.clone();
            // api数据桶副本
            let api_histogram_clone = api_histogram.clone();
            // api成功数量统计副本
            let api_successful_requests_clone = api_successful_requests.clone();
            // api总统计数量统计副本
            let api_total_requests_clone = api_total_requests.clone();
            // api最大响应时间副本
            let api_max_response_time_clone = api_max_response_time.clone();
            // api最小响应时间副本
            let api_min_response_time_clone = api_min_response_time.clone();
            // api错误数量统计副本
            let api_err_count_clone = api_err_count.clone();
            // api并发数统计副本
            let api_concurrent_number_clone = api_concurrent_number.clone();
            // api结果副本
            let api_result_clone = api_result.clone();
            // api吞吐量副本
            let api_total_response_size_clone = api_total_response_size.clone();
            // 总请求数记录副本
            let total_requests_clone = Arc::clone(&total_requests);
            // 每个接口端点副本
            let endpoint_clone = Arc::clone(&endpoint_arc);
            // 将新url替换到每个接口中
            endpoint_clone.lock().await.url = api_url.clone();
            // 最大响应时间副本
            let max_response_time_clone = max_response_time.clone();
            // 响应大小统计副本
            let total_response_size_clone = total_response_size.clone();
            // 最小响应时间副本
            let min_response_time_clone = min_response_time.clone();
            // 错误次数副本
            let err_count_clone = err_count.clone();
            // 并发数统计副本
            let concurrent_number_clone = concurrent_number.clone();
            // 断言错误副本
            let assert_errors_clone = assert_errors.clone();
            // 成功次数副本
            let successful_requests_clone = successful_requests.clone();
            // http错误副本
            let http_errors_clone = http_errors.clone();
            // results副本
            let results_clone = results_arc.clone();
            // 并发控制器副本
            let controller_clone = controller.clone();
            // 全局提取字典副本
            let extract_map_arc_clone = Arc::clone(&extract_map_arc);
            // 断言通道副本
            let tx_assert_clone = tx_assert.clone();
            // http客户端副本
            let client_clone = client.clone();
            // 开启并发
            let handle: JoinHandle<Result<(), Error>> = tokio::spawn(async move {
                let semaphore = controller_clone.get_semaphore();
                let _permit = semaphore.acquire().await.expect("获取信号量许可失败");
                // 统计并发数
                *api_concurrent_number_clone.lock().await += 1;
                *concurrent_number_clone.lock().await += 1;
                'RETRY: while Instant::now() < test_end {
                    // 设置api的提取器
                    let mut api_extract_b_tree_map = BTreeMap::new();
                    // 将全局字典加入到api字典
                    api_extract_b_tree_map.extend(extract_map_arc_clone.lock().await.clone());

                    // 接口初始化副本
                    let api_setup_clone = endpoint_clone.lock().await.setup_options.clone();
                    // 接口前置初始化
                    if let Some(setup_options) = api_setup_clone {
                        is_need_render_template = true;
                        match setup::start_setup(
                            setup_options,
                            api_extract_b_tree_map.clone(),
                            client_clone.clone(),
                        )
                        .await
                        {
                            Ok(res) => {
                                if let Some(extract) = res {
                                    api_extract_b_tree_map.extend(extract);
                                };
                            }
                            Err(e) => {
                                eprintln!(
                                    "接口-{:?}初始化失败,1秒后重试!!: {:?}",
                                    api_name_clone.clone(),
                                    e.to_string()
                                );
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                continue 'RETRY;
                            }
                        };
                    }
                    // 总请求数
                    *total_requests_clone.lock().await += 1;
                    // api请求数
                    *api_total_requests_clone.lock().await += 1;
                    // 请求方法副本
                    let method_clone = endpoint_clone.lock().await.method.clone();
                    // json副本
                    let json_obj_clone = endpoint_clone.lock().await.json.clone();
                    // form副本
                    let form_data_clone = endpoint_clone.lock().await.form_data.clone();
                    // headers副本
                    let headers_clone = endpoint_clone.lock().await.headers.clone();
                    // cookie副本
                    let cookie_clone = endpoint_clone.lock().await.cookies.clone();
                    // 断言副本
                    let assert_options_clone = endpoint_clone.lock().await.assert_options.clone();
                    // 思考时间副本
                    let think_time_clone = endpoint_clone.lock().await.think_time_option.clone();
                    // 构建请求方式
                    let method = Method::from_str(&method_clone.to_uppercase())
                        .map_err(|_| Error::msg("构建请求方法失败"))?;
                    // 构建请求
                    let mut request = client_clone.request(method, api_url_clone.clone());
                    // 构建请求头
                    let mut headers = HeaderMap::new();
                    // headers.insert(USER_AGENT, user_agent_clone.parse()?);
                    if let Some(headers_map) = headers_clone {
                        headers.extend(headers_map.iter().map(|(k, v)| {
                            let header_name = k.parse::<HeaderName>().expect("无效的header名称");
                            match is_need_render_template {
                                true => {
                                    // 将header的value模板进行填充
                                    let handlebars = Handlebars::new();
                                    let new_val = match handlebars
                                        .render_template(v, &json!(api_extract_b_tree_map))
                                    {
                                        Ok(v) => v,
                                        Err(e) => {
                                            eprintln!("{:?}", e);
                                            v.to_string()
                                        }
                                    };
                                    let header_value =
                                        new_val.parse::<HeaderValue>().expect("无效的header值");
                                    (header_name.clone(), header_value)
                                }
                                false => {
                                    let header_value =
                                        v.parse::<HeaderValue>().expect("无效的header值");
                                    (header_name, header_value)
                                }
                            }
                        }));
                    }
                    // 构建cookies
                    if let Some(ref source) = cookie_clone {
                        let cookie_val = match is_need_render_template {
                            true => {
                                // 使用模版替换cookies
                                let handlebars = Handlebars::new();
                                match handlebars
                                    .render_template(source, &json!(api_extract_b_tree_map))
                                {
                                    Ok(c) => c,
                                    Err(e) => {
                                        eprintln!("{:?}", e);
                                        source.to_string()
                                    }
                                }
                            }
                            false => source.to_string(),
                        };
                        // println!("{:?}", cookie_val);
                        match HeaderValue::from_str(&cookie_val) {
                            Ok(h) => {
                                headers.insert(COOKIE, h);
                            }
                            Err(e) => return Err(Error::msg(format!("设置cookie失败:{:?}", e))),
                        }
                    }
                    request = request.headers(headers);
                    // 构建json请求
                    if let Some(json_value) = json_obj_clone {
                        let json_val = match is_need_render_template {
                            true => {
                                // 将json转为字符串，并且将模版填充
                                let handlebars = Handlebars::new();
                                let json_string = match handlebars.render_template(
                                    &*json_value.to_string(),
                                    &json!(api_extract_b_tree_map),
                                ) {
                                    Ok(j) => j,
                                    Err(e) => {
                                        eprintln!("{:?}", e);
                                        json_value.to_string()
                                    }
                                };
                                match Value::from_str(&*json_string) {
                                    Ok(val) => val,
                                    Err(e) => {
                                        return Err(Error::msg(format!(
                                            "转换json失败:{:?}, 原始json: {:?}",
                                            e,
                                            json_string.to_string()
                                        )))
                                    }
                                }
                            }
                            false => json_value,
                        };
                        // println!("{:?}", json_val);
                        if verbose {
                            println!("json:{:?}", json_val);
                        };
                        request = request.json(&json_val);
                    }
                    // 构建form表单
                    if let Some(mut form_data) = form_data_clone {
                        if is_need_render_template {
                            // 将模版填充
                            form_data.iter_mut().for_each(|(_key, value)| {
                                let handlebars = Handlebars::new();
                                let new_val = match handlebars
                                    .render_template(value, &json!(api_extract_b_tree_map))
                                {
                                    Ok(v) => v,
                                    Err(e) => {
                                        eprintln!("{:?}", e);
                                        value.to_string()
                                    }
                                };
                                *value = new_val;
                            })
                        };
                        request = request.form(&form_data);
                    };
                    if verbose {
                        println!("{:?}", request);
                    };
                    // println!("{:?}", request);
                    // 如果有思考时间，暂时不发送请求，先等待
                    if let Some(think_time) = think_time_clone {
                        match think_time.min_millis <= think_time.max_millis {
                            true => {
                                let mut rng = StdRng::from_entropy();
                                let tt =
                                    rng.gen_range(think_time.min_millis..=think_time.max_millis);
                                if verbose {
                                    println!("思考时间：{:?}", tt);
                                }
                                tokio::time::sleep(Duration::from_millis(tt)).await;
                            }
                            false => {
                                eprintln!("最小思考时间大于最大思考时间，该配置不生效!")
                            }
                        }
                    }
                    // 记录开始时间
                    let start = Instant::now();
                    // 发送请求
                    match request.send().await {
                        Ok(response) => {
                            let status = response.status();
                            match status {
                                // 正确的状态码
                                StatusCode::OK
                                | StatusCode::CREATED
                                | StatusCode::ACCEPTED
                                | StatusCode::NON_AUTHORITATIVE_INFORMATION
                                | StatusCode::NO_CONTENT
                                | StatusCode::RESET_CONTENT
                                | StatusCode::PARTIAL_CONTENT
                                | StatusCode::MULTI_STATUS
                                | StatusCode::ALREADY_REPORTED
                                | StatusCode::IM_USED
                                | StatusCode::MULTIPLE_CHOICES
                                | StatusCode::MOVED_PERMANENTLY
                                | StatusCode::FOUND
                                | StatusCode::SEE_OTHER
                                | StatusCode::NOT_MODIFIED
                                | StatusCode::USE_PROXY
                                | StatusCode::TEMPORARY_REDIRECT
                                | StatusCode::PERMANENT_REDIRECT => {
                                    /// # 请求成功的情况
                                    // 响应时间
                                    let duration = start.elapsed().as_millis() as u64;
                                    // api统计桶
                                    let mut api_histogram = api_histogram_clone.lock().await;
                                    // 最大请求时间
                                    let mut max_rt = max_response_time_clone.lock().await;
                                    *max_rt = (*max_rt).max(duration);
                                    // api最大请求时间
                                    let mut api_max_rt = api_max_response_time_clone.lock().await;
                                    *api_max_rt = (*api_max_rt).max(duration);
                                    // 最小响应时间
                                    let mut min_rt = min_response_time_clone.lock().await;
                                    *min_rt = (*min_rt).min(duration);
                                    // api最小响应时间
                                    let mut api_min_rt = api_min_response_time_clone.lock().await;
                                    *api_min_rt = (*api_min_rt).min(duration);
                                    // 将数据放入全局统计桶
                                    if let Err(e) = histogram_clone.lock().await.increment(duration)
                                    {
                                        eprintln!("histogram设置数据错误:{:?}", e)
                                    };
                                    // 将数据放入api统计桶
                                    if let Err(e) = api_histogram.increment(duration) {
                                        eprintln!("api histogram设置错误:{:?}", e)
                                    }
                                    // 获取响应头
                                    let resp_headers = response.headers();
                                    // 计算响应头大小
                                    let headers_size =
                                        resp_headers.iter().fold(0, |acc, (name, value)| {
                                            acc + name.as_str().len()
                                                + 2
                                                + value.as_bytes().len()
                                                + 2
                                        });
                                    // 将响应头的大小加入到总大小中
                                    {
                                        let mut total_size = total_response_size_clone.lock().await;
                                        *total_size += headers_size as u64;
                                        let mut api_total_size =
                                            api_total_response_size_clone.lock().await;
                                        *api_total_size += headers_size as u64;
                                    }
                                    // 响应流
                                    let mut stream = response.bytes_stream();
                                    // 响应体
                                    let mut body_bytes = Vec::new();
                                    while let Some(item) = stream.next().await {
                                        match item {
                                            Ok(chunk) => {
                                                // 获取当前的chunk
                                                let mut total_size =
                                                    total_response_size_clone.lock().await;
                                                *total_size += chunk.len() as u64;
                                                let mut api_total_size =
                                                    api_total_response_size_clone.lock().await;
                                                *api_total_size += chunk.len() as u64;
                                                body_bytes.extend_from_slice(&chunk);
                                            }
                                            Err(e) => {
                                                *api_err_count_clone.lock().await += 1;
                                                *err_count_clone.lock().await += 1;
                                                http_errors_clone
                                                    .lock()
                                                    .await
                                                    .increment(
                                                        0,
                                                        format!("获取响应流失败::{:?}", e),
                                                        api_url_clone.clone(),
                                                    )
                                                    .await;
                                                break;
                                            }
                                        };
                                    }
                                    if verbose {
                                        let body_bytes_clone = body_bytes.clone();
                                        let buffer = String::from_utf8(body_bytes_clone)
                                            .expect("无法转换响应体为字符串");
                                        println!("{:+?}", buffer);
                                    }
                                    // 断言
                                    match assert_options_clone {
                                        Some(assert_options) => {
                                            // 没有获取到响应体，就不进行断言
                                            if body_bytes.clone().len() > 0 {
                                                // 一次性通道，用于确定断言任务被消费完成后再进行数据同步
                                                let (oneshot_tx, oneshot_rx) = oneshot::channel();
                                                // 实例化任务
                                                let task = AssertTask {
                                                    assert_options: assert_options.clone(),
                                                    body_bytes,
                                                    verbose,
                                                    err_count: err_count_clone.clone(),
                                                    api_err_count: api_err_count_clone.clone(),
                                                    assert_errors: assert_errors_clone.clone(),
                                                    endpoint: endpoint_clone.clone(),
                                                    api_name: api_name_clone.clone(),
                                                    successful_requests: successful_requests_clone
                                                        .clone(),
                                                    api_successful_requests:
                                                        api_successful_requests_clone.clone(),
                                                    completion_signal: oneshot_tx,
                                                };
                                                // 存在断言数据将任务生产到队列中
                                                tx_assert_clone
                                                    .send(task)
                                                    .await
                                                    .expect("生产断言任务失败");
                                                // 等待任务消费完成后再进行后面的赋值操作，用于数据同步
                                                oneshot_rx.await.expect("任务完成信号失败");
                                            };
                                        }
                                        None => {
                                            // 没有断言的时候将成功数据+1
                                            *successful_requests_clone.lock().await += 1;
                                            *api_successful_requests_clone.lock().await += 1;
                                        }
                                    };
                                    // 给结果赋值
                                    let api_total_data_bytes =
                                        *api_total_response_size_clone.lock().await;
                                    let api_total_data_kb = api_total_data_bytes as f64 / 1024f64;
                                    let api_total_requests =
                                        api_total_requests_clone.lock().await.clone();
                                    let api_success_requests =
                                        api_successful_requests_clone.lock().await.clone();
                                    let api_rps = api_total_requests as f64
                                        / (Instant::now() - test_start).as_secs_f64();
                                    let api_success_rate = api_success_requests as f64
                                        / api_total_requests as f64
                                        * 100.0;
                                    let throughput_per_second_kb = api_total_data_kb
                                        / (Instant::now() - test_start).as_secs_f64();

                                    let mut api_res = api_result_clone.lock().await;
                                    api_res.response_time_95 =
                                        *api_histogram.percentile(95.0)?.range().start();
                                    api_res.response_time_99 =
                                        *api_histogram.percentile(99.0)?.range().start();
                                    api_res.median_response_time =
                                        *api_histogram.percentile(50.0)?.range().start();
                                    api_res.max_response_time = *api_max_rt;
                                    api_res.min_response_time = *api_min_rt;
                                    api_res.total_requests = api_total_requests;
                                    api_res.total_data_kb = api_total_data_kb;
                                    api_res.rps = api_rps;
                                    api_res.success_rate = api_success_rate;
                                    api_res.err_count = *api_err_count_clone.lock().await;
                                    api_res.throughput_per_second_kb = throughput_per_second_kb;
                                    api_res.error_rate = api_res.err_count as f64
                                        / api_res.total_requests as f64
                                        * 100.0;
                                    api_res.method = method_clone.clone().to_uppercase();
                                    api_res.concurrent_number =
                                        *api_concurrent_number_clone.lock().await;
                                    // 向最终结果中添加数据
                                    let mut res = results_clone.lock().await;
                                    match index < res.len() {
                                        true => {
                                            res[index] = api_res.clone();
                                        }
                                        false => {
                                            eprintln!("results索引越界");
                                        }
                                    };
                                    // println!("res:{:?}", res);
                                }
                                // 状态码错误
                                _ => {
                                    // 响应时间
                                    let duration = start.elapsed().as_millis() as u64;
                                    *err_count_clone.lock().await += 1;
                                    *api_err_count_clone.lock().await += 1;
                                    let status_code = u16::from(response.status());
                                    let err_msg = format!("HTTP 错误: 状态码 {}", status_code);
                                    let url = response.url().to_string();
                                    http_errors_clone
                                        .lock()
                                        .await
                                        .increment(status_code, err_msg, url)
                                        .await;
                                    if verbose {
                                        println!(
                                            "{:?}-HTTP 错误: 状态码 {:?}",
                                            api_name_clone, status_code
                                        )
                                    }
                                    let mut api_histogram = api_histogram_clone.lock().await;
                                    // 最大请求时间
                                    let mut max_rt = max_response_time_clone.lock().await;
                                    *max_rt = (*max_rt).max(duration);
                                    // api最大请求时间
                                    let mut api_max_rt = api_max_response_time_clone.lock().await;
                                    *api_max_rt = (*api_max_rt).max(duration);
                                    // 最小响应时间
                                    let mut min_rt = min_response_time_clone.lock().await;
                                    *min_rt = (*min_rt).min(duration);
                                    // api最小响应时间
                                    let mut api_min_rt = api_min_response_time_clone.lock().await;
                                    *api_min_rt = (*api_min_rt).min(duration);
                                    // 将数据放入全局统计桶
                                    if let Err(e) = histogram_clone.lock().await.increment(duration)
                                    {
                                        eprintln!("histogram设置数据错误:{:?}", e)
                                    };
                                    // 将数据放入api统计桶
                                    if let Err(e) = api_histogram.increment(duration) {
                                        eprintln!("api histogram设置错误:{:?}", e)
                                    }
                                    // 获取响应头
                                    let resp_headers = response.headers();
                                    // 计算响应头大小
                                    let headers_size =
                                        resp_headers.iter().fold(0, |acc, (name, value)| {
                                            acc + name.as_str().len()
                                                + 2
                                                + value.as_bytes().len()
                                                + 2
                                        });
                                    // 将响应头的大小加入到总大小中
                                    {
                                        let mut total_size = total_response_size_clone.lock().await;
                                        *total_size += headers_size as u64;
                                        let mut api_total_size =
                                            api_total_response_size_clone.lock().await;
                                        *api_total_size += headers_size as u64;
                                    }
                                    // 响应流
                                    let mut stream = response.bytes_stream();
                                    // 响应体
                                    let mut body_bytes = Vec::new();
                                    while let Some(item) = stream.next().await {
                                        match item {
                                            Ok(chunk) => {
                                                // 获取当前的chunk
                                                let mut total_size =
                                                    total_response_size_clone.lock().await;
                                                *total_size += chunk.len() as u64;
                                                let mut api_total_size =
                                                    api_total_response_size_clone.lock().await;
                                                *api_total_size += chunk.len() as u64;
                                                body_bytes.extend_from_slice(&chunk);
                                            }
                                            Err(e) => {
                                                *api_err_count_clone.lock().await += 1;
                                                *err_count_clone.lock().await += 1;
                                                http_errors_clone
                                                    .lock()
                                                    .await
                                                    .increment(
                                                        0,
                                                        format!("获取响应流失败::{:?}", e),
                                                        api_url_clone.clone(),
                                                    )
                                                    .await;
                                                break;
                                            }
                                        };
                                    }
                                    if verbose {
                                        let body_bytes_clone = body_bytes.clone();
                                        let buffer = String::from_utf8(body_bytes_clone)
                                            .expect("无法转换响应体为字符串");
                                        println!("{:+?}", buffer);
                                    }
                                    let api_total_data_bytes =
                                        *api_total_response_size_clone.lock().await;
                                    let api_total_data_kb = api_total_data_bytes as f64 / 1024f64;
                                    let api_total_requests =
                                        api_total_requests_clone.lock().await.clone();
                                    let api_success_requests =
                                        api_successful_requests_clone.lock().await.clone();
                                    let api_rps = api_total_requests as f64
                                        / (Instant::now() - test_start).as_secs_f64();
                                    let api_success_rate = api_success_requests as f64
                                        / api_total_requests as f64
                                        * 100.0;
                                    let throughput_per_second_kb = api_total_data_kb
                                        / (Instant::now() - test_start).as_secs_f64();
                                    // 给结果赋值
                                    let mut api_res = api_result_clone.lock().await;
                                    api_res.response_time_95 =
                                        *api_histogram.percentile(95.0)?.range().start();
                                    api_res.response_time_99 =
                                        *api_histogram.percentile(99.0)?.range().start();
                                    api_res.median_response_time =
                                        *api_histogram.percentile(50.0)?.range().start();
                                    api_res.max_response_time = *api_max_rt;
                                    api_res.min_response_time = *api_min_rt;
                                    api_res.total_requests = api_total_requests;
                                    api_res.total_data_kb = api_total_data_kb;
                                    api_res.rps = api_rps;
                                    api_res.success_rate = api_success_rate;
                                    api_res.err_count = *api_err_count_clone.lock().await;
                                    api_res.throughput_per_second_kb = throughput_per_second_kb;
                                    api_res.error_rate = api_res.err_count as f64
                                        / api_res.total_requests as f64
                                        * 100.0;
                                    api_res.method = method_clone.clone().to_uppercase();
                                    api_res.concurrent_number =
                                        *api_concurrent_number_clone.lock().await;
                                    // 向最终结果中添加数据
                                    let mut res = results_clone.lock().await;
                                    match index < res.len() {
                                        true => {
                                            res[index] = api_res.clone();
                                        }
                                        false => {
                                            eprintln!("results索引越界");
                                        }
                                    };
                                }
                            }
                        }
                        Err(e) => {
                            *err_count_clone.lock().await += 1;
                            *api_err_count_clone.lock().await += 1;
                            let status_code: u16;
                            match e.status() {
                                None => {
                                    status_code = 0;
                                }
                                Some(code) => {
                                    status_code = u16::from(code);
                                }
                            }
                            let err_msg = e.to_string();
                            http_errors_clone
                                .lock()
                                .await
                                .increment(status_code, err_msg, api_url_clone.clone())
                                .await;
                        }
                    }
                }
                Ok(())
            });
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
            false,
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
