use crate::core::concurrency_controller::ConcurrencyController;
use crate::core::setup;
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::assert_error_stats::AssertErrorStats;
use crate::models::assert_task::AssertTask;
use crate::models::http_error_stats::HttpErrorStats;
use crate::models::result::ApiResult;
use anyhow::Error;
use futures::StreamExt;
use handlebars::Handlebars;
use histogram::Histogram;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error as std_error;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::Sender;
use tokio::sync::{oneshot, Mutex};

pub(crate) async fn start_concurrency(
    client: Client,
    controller_arc: Arc<ConcurrencyController>,
    api_concurrent_number_arc: Arc<AtomicUsize>,
    concurrent_number_arc: Arc<AtomicUsize>,
    extract_map_arc: Arc<Mutex<BTreeMap<String, Value>>>,
    endpoint_arc: Arc<Mutex<ApiEndpoint>>,
    total_requests_arc: Arc<AtomicUsize>,
    api_total_requests_arc: Arc<AtomicUsize>,
    histogram_arc: Arc<Mutex<Histogram>>,
    api_histogram_arc: Arc<Mutex<Histogram>>,
    max_response_time_arc: Arc<Mutex<u64>>,
    api_max_response_time_arc: Arc<Mutex<u64>>,
    min_response_time_arc: Arc<Mutex<u64>>,
    api_min_response_time_arc: Arc<Mutex<u64>>,
    total_response_size_arc: Arc<AtomicUsize>,
    api_total_response_size_arc: Arc<AtomicUsize>,
    api_err_count_arc: Arc<AtomicUsize>,
    successful_requests_arc: Arc<AtomicUsize>,
    err_count_arc: Arc<AtomicUsize>,
    http_errors_arc: Arc<Mutex<HttpErrorStats>>,
    assert_errors_arc: Arc<Mutex<AssertErrorStats>>,
    api_successful_requests_arc: Arc<AtomicUsize>,
    api_result_arc: Arc<Mutex<ApiResult>>,
    results_arc: Arc<Mutex<Vec<ApiResult>>>,
    tx_assert: Sender<AssertTask>,
    test_start: Instant,
    test_end: Instant,
    is_need_render_template: bool,
    verbose: bool,
    index: usize,
) -> Result<(), Error> {
    let mut is_need_render = is_need_render_template;
    let semaphore = controller_arc.get_semaphore();
    let _permit = semaphore.acquire().await.expect("获取信号量许可失败");
    // 统计并发数
    api_concurrent_number_arc.fetch_add(1, Ordering::Relaxed);
    concurrent_number_arc.fetch_add(1, Ordering::Relaxed);
    'RETRY: while Instant::now() < test_end {
        // 设置api的提取器
        let mut api_extract_b_tree_map = BTreeMap::new();
        // 将全局字典加入到api字典
        api_extract_b_tree_map.extend(extract_map_arc.lock().await.clone());

        // 接口初始化副本
        let api_setup_clone = endpoint_arc.lock().await.setup_options.clone();
        // 接口前置初始化
        if let Some(setup_options) = api_setup_clone {
            is_need_render = true;
            match setup::start_setup(
                setup_options,
                api_extract_b_tree_map.clone(),
                client.clone(),
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
                        endpoint_arc.lock().await.name.clone(),
                        e.to_string()
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue 'RETRY;
                }
            };
        }
        // api名称副本
        let api_name_clone = endpoint_arc.lock().await.name.clone();
        // url副本
        let url_clone = endpoint_arc.lock().await.url.clone();
        // 请求方法副本
        let method_clone = endpoint_arc.lock().await.method.clone();
        // json副本
        let json_obj_clone = endpoint_arc.lock().await.json.clone();
        // form副本
        let form_data_clone = endpoint_arc.lock().await.form_data.clone();
        // headers副本
        let headers_clone = endpoint_arc.lock().await.headers.clone();
        // cookie副本
        let cookie_clone = endpoint_arc.lock().await.cookies.clone();
        // 断言副本
        let assert_options_clone = endpoint_arc.lock().await.assert_options.clone();
        // 思考时间副本
        let think_time_clone = endpoint_arc.lock().await.think_time_option.clone();
        // 构建请求方式
        let method = Method::from_str(&method_clone.to_uppercase())
            .map_err(|_| Error::msg("构建请求方法失败"))?;
        // 构建请求
        let mut request = client.request(method, endpoint_arc.lock().await.url.clone());
        // 构建请求头
        let mut headers = HeaderMap::new();
        if let Some(headers_map) = headers_clone {
            headers.extend(headers_map.iter().map(|(k, v)| {
                let header_name = k.parse::<HeaderName>().expect("无效的header名称");
                match is_need_render_template {
                    true => {
                        // 将header的value模板进行填充
                        let handlebars = Handlebars::new();
                        let new_val =
                            match handlebars.render_template(v, &json!(api_extract_b_tree_map)) {
                                Ok(v) => v,
                                Err(e) => {
                                    eprintln!("{:?}", e);
                                    v.to_string()
                                }
                            };
                        let header_value = new_val.parse::<HeaderValue>().expect("无效的header值");
                        (header_name.clone(), header_value)
                    }
                    false => {
                        let header_value = v.parse::<HeaderValue>().expect("无效的header值");
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
                    match handlebars.render_template(source, &json!(api_extract_b_tree_map)) {
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
                    let json_string = match handlebars
                        .render_template(&*json_value.to_string(), &json!(api_extract_b_tree_map))
                    {
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
            if is_need_render {
                // 将模版填充
                form_data.iter_mut().for_each(|(_key, value)| {
                    let handlebars = Handlebars::new();
                    let new_val =
                        match handlebars.render_template(value, &json!(api_extract_b_tree_map)) {
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
                    let tt = rng.gen_range(think_time.min_millis..=think_time.max_millis);
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
                // 总请求数
                total_requests_arc.fetch_add(1, Ordering::Relaxed);
                // api请求数
                api_total_requests_arc.fetch_add(1, Ordering::Relaxed);
                // 获取状态码
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
                        // 请求成功的情况
                        // 响应时间
                        let duration = start.elapsed().as_millis() as u64;
                        // api统计桶
                        let mut api_histogram = api_histogram_arc.lock().await;
                        // 最大请求时间
                        let mut max_rt = max_response_time_arc.lock().await;
                        *max_rt = (*max_rt).max(duration);
                        // api最大请求时间
                        let mut api_max_rt = api_max_response_time_arc.lock().await;
                        *api_max_rt = (*api_max_rt).max(duration);
                        // 最小响应时间
                        let mut min_rt = min_response_time_arc.lock().await;
                        *min_rt = (*min_rt).min(duration);
                        // api最小响应时间
                        let mut api_min_rt = api_min_response_time_arc.lock().await;
                        *api_min_rt = (*api_min_rt).min(duration);
                        // 将数据放入全局统计桶
                        if let Err(e) = histogram_arc.lock().await.increment(duration) {
                            eprintln!("histogram设置数据错误:{:?}", e)
                        };
                        // 将数据放入api统计桶
                        if let Err(e) = api_histogram.increment(duration) {
                            eprintln!("api histogram设置错误:{:?}", e)
                        }
                        // 获取响应头
                        let resp_headers = response.headers();
                        // 计算响应头大小
                        let headers_size = resp_headers.iter().fold(0, |acc, (name, value)| {
                            acc + name.as_str().len() + 2 + value.as_bytes().len() + 2
                        });
                        // 将响应头的大小加入到总大小中
                        {
                            total_response_size_arc.fetch_add(headers_size, Ordering::Relaxed);
                            api_total_response_size_arc.fetch_add(headers_size, Ordering::Relaxed);
                        }
                        // 响应流
                        let mut stream = response.bytes_stream();
                        // 响应体
                        let mut body_bytes = Vec::new();
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(chunk) => {
                                    // 获取当前的chunk
                                    total_response_size_arc
                                        .fetch_add(chunk.len(), Ordering::Relaxed);
                                    api_total_response_size_arc
                                        .fetch_add(chunk.len(), Ordering::Relaxed);
                                    body_bytes.extend_from_slice(&chunk);
                                }
                                Err(e) => {
                                    api_err_count_arc.fetch_add(1, Ordering::Relaxed);
                                    err_count_arc.fetch_add(1, Ordering::Relaxed);
                                    http_errors_arc
                                        .lock()
                                        .await
                                        .increment(
                                            api_name_clone.clone(),
                                            url_clone.clone(),
                                            match e.status() {
                                                None => 0u16,
                                                Some(status_code) => status_code.as_u16(),
                                            },
                                            e.to_string(),
                                            match e.source() {
                                                None => "-".to_string(),
                                                Some(source) => source.to_string(),
                                            },
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
                                        err_count: err_count_arc.clone(),
                                        api_err_count: api_err_count_arc.clone(),
                                        assert_errors: assert_errors_arc.clone(),
                                        endpoint: endpoint_arc.clone(),
                                        api_name: endpoint_arc.lock().await.name.clone(),
                                        successful_requests: successful_requests_arc.clone(),
                                        api_successful_requests: api_successful_requests_arc
                                            .clone(),
                                        completion_signal: oneshot_tx,
                                    };
                                    // 存在断言数据将任务生产到队列中
                                    tx_assert.send(task).await.expect("生产断言任务失败");
                                    // 等待任务消费完成后再进行后面的赋值操作，用于数据同步
                                    oneshot_rx.await.expect("任务完成信号失败");
                                };
                            }
                            None => {
                                // 没有断言的时候将成功数据+1
                                successful_requests_arc.fetch_add(1, Ordering::Relaxed);
                                api_successful_requests_arc.fetch_add(1, Ordering::Relaxed);
                            }
                        };
                        // 给结果赋值
                        {
                            let api_total_data_bytes =
                                api_total_response_size_arc.load(Ordering::SeqCst);
                            let api_total_data_kb = api_total_data_bytes as f64 / 1024f64;
                            let api_total_requests =
                                api_total_requests_arc.load(Ordering::SeqCst) as u64;
                            let api_success_requests =
                                api_successful_requests_arc.load(Ordering::SeqCst);
                            let api_success_rate =
                                api_success_requests as f64 / api_total_requests as f64 * 100.0;
                            let throughput_per_second_kb =
                                api_total_data_kb / (Instant::now() - test_start).as_secs_f64();

                            let mut api_res = api_result_arc.lock().await;
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
                            api_res.success_rate = api_success_rate;
                            api_res.err_count = api_err_count_arc.load(Ordering::SeqCst) as i32;
                            api_res.throughput_per_second_kb = throughput_per_second_kb;
                            api_res.error_rate =
                                api_res.err_count as f64 / api_res.total_requests as f64 * 100.0;
                            api_res.method = method_clone.clone().to_uppercase();
                            api_res.concurrent_number =
                                api_concurrent_number_arc.load(Ordering::SeqCst) as i32;
                            // 向最终结果中添加数据
                            let mut res = results_arc.lock().await;
                            match index < res.len() {
                                true => {
                                    res[index] = api_res.clone();
                                }
                                false => {
                                    eprintln!("results索引越界");
                                }
                            };
                        }
                        // println!("res:{:?}", res);
                    }
                    // 状态码错误
                    _ => {
                        // 响应时间
                        let duration = start.elapsed().as_millis() as u64;
                        err_count_arc.fetch_add(1, Ordering::Relaxed);
                        api_err_count_arc.fetch_add(1, Ordering::Relaxed);
                        let status_code = u16::from(response.status());
                        let mut api_histogram = api_histogram_arc.lock().await;
                        // 最大请求时间
                        let mut max_rt = max_response_time_arc.lock().await;
                        *max_rt = (*max_rt).max(duration);
                        // api最大请求时间
                        let mut api_max_rt = api_max_response_time_arc.lock().await;
                        *api_max_rt = (*api_max_rt).max(duration);
                        // 最小响应时间
                        let mut min_rt = min_response_time_arc.lock().await;
                        *min_rt = (*min_rt).min(duration);
                        // api最小响应时间
                        let mut api_min_rt = api_min_response_time_arc.lock().await;
                        *api_min_rt = (*api_min_rt).min(duration);
                        // 将数据放入全局统计桶
                        if let Err(e) = histogram_arc.lock().await.increment(duration) {
                            eprintln!("histogram设置数据错误:{:?}", e)
                        };
                        // 将数据放入api统计桶
                        if let Err(e) = api_histogram.increment(duration) {
                            eprintln!("api histogram设置错误:{:?}", e)
                        }
                        // 获取响应头
                        let resp_headers = response.headers();
                        // 计算响应头大小
                        let headers_size = resp_headers.iter().fold(0, |acc, (name, value)| {
                            acc + name.as_str().len() + 2 + value.as_bytes().len() + 2
                        });
                        // 将响应头的大小加入到总大小中
                        {
                            total_response_size_arc.fetch_add(headers_size, Ordering::Relaxed);
                            api_total_response_size_arc.fetch_add(headers_size, Ordering::Relaxed);
                        }
                        // 响应流
                        let mut stream = response.bytes_stream();
                        // 响应体
                        let mut body_bytes = Vec::new();
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(chunk) => {
                                    // 获取当前的chunk
                                    total_response_size_arc
                                        .fetch_add(chunk.len(), Ordering::Relaxed);
                                    api_total_response_size_arc
                                        .fetch_add(chunk.len(), Ordering::Relaxed);
                                    body_bytes.extend_from_slice(&chunk);
                                }
                                Err(e) => {
                                    api_err_count_arc.fetch_add(1, Ordering::Relaxed);
                                    err_count_arc.fetch_add(1, Ordering::Relaxed);
                                    http_errors_arc
                                        .lock()
                                        .await
                                        .increment(
                                            api_name_clone.clone(),
                                            url_clone.clone(),
                                            match e.status() {
                                                None => 0u16,
                                                Some(status_code) => status_code.as_u16(),
                                            },
                                            e.to_string(),
                                            match e.source() {
                                                None => "-".to_string(),
                                                Some(source) => source.to_string(),
                                            },
                                        )
                                        .await;
                                    break;
                                }
                            };
                        }
                        // 响应体副本
                        let body_bytes_clone = body_bytes.clone();
                        // 将bytes转换为string
                        let buffer =
                            String::from_utf8(body_bytes_clone).expect("无法转换响应体为字符串");
                        eprintln!("{:+?}", buffer);
                        // 获取需要等待的对象
                        let api_total_data_bytes =
                            api_total_response_size_arc.load(Ordering::SeqCst);
                        let api_total_data_kb = api_total_data_bytes as f64 / 1024f64;
                        let api_total_requests =
                            api_total_requests_arc.load(Ordering::SeqCst) as u64;
                        let api_success_requests =
                            api_successful_requests_arc.load(Ordering::SeqCst);
                        let api_success_rate =
                            api_success_requests as f64 / api_total_requests as f64 * 100.0;
                        let throughput_per_second_kb =
                            api_total_data_kb / (Instant::now() - test_start).as_secs_f64();
                        let err_msg =
                            format!("HTTP 错误: 状态码 {:?}, body:{:?}", status_code, buffer);
                        http_errors_arc
                            .lock()
                            .await
                            .increment(
                                api_name_clone.clone(),
                                url_clone.clone(),
                                status.as_u16(),
                                err_msg,
                                "-".to_string(),
                            )
                            .await;
                        if verbose {
                            println!(
                                "{:?}-HTTP 错误: 状态码 {:?}, 响应体：{:?}",
                                endpoint_arc.lock().await.name.clone(),
                                status_code,
                                buffer
                            )
                        }
                        // 给结果赋值
                        {
                            let mut api_res = api_result_arc.lock().await;
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
                            api_res.success_rate = api_success_rate;
                            api_res.err_count = api_err_count_arc.load(Ordering::SeqCst) as i32;
                            api_res.throughput_per_second_kb = throughput_per_second_kb;
                            api_res.error_rate =
                                api_res.err_count as f64 / api_res.total_requests as f64 * 100.0;
                            api_res.method = method_clone.clone().to_uppercase();
                            api_res.concurrent_number =
                                api_concurrent_number_arc.load(Ordering::SeqCst) as i32;
                            // 向最终结果中添加数据
                            let mut res = results_arc.lock().await;
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
            }
            Err(e) => {
                // 总请求数
                total_requests_arc.fetch_add(1, Ordering::Relaxed);
                // api请求数
                api_total_requests_arc.fetch_add(1, Ordering::Relaxed);
                err_count_arc.fetch_add(1, Ordering::Relaxed);
                api_err_count_arc.fetch_add(1, Ordering::Relaxed);
                let status_code: u16;
                match e.status() {
                    None => {
                        status_code = 0;
                    }
                    Some(code) => {
                        status_code = u16::from(code);
                    }
                }

                let err = dbg!(e);
                let err_source = match err.source() {
                    None => "None".to_string(),
                    Some(source) => source.to_string(),
                };
                http_errors_arc
                    .lock()
                    .await
                    .increment(
                        endpoint_arc.lock().await.name.clone(),
                        url_clone.clone(),
                        status_code,
                        err.to_string(),
                        err_source,
                    )
                    .await;
            }
        }
    }
    Ok(())
}
