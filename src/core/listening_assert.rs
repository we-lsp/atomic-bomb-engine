use crate::models::assert_task::AssertTask;
use jsonpath_lib::select;
use serde_json::Value;
use tokio::sync::mpsc;

// todo: 断言可以使用模板
pub async fn listening_assert(mut rx: mpsc::Receiver<AssertTask>) {
    loop {
        tokio::select! {
            Some(task) = rx.recv() => {
                let mut assertion_failed = false;
                let json_value: Option<Value> = match serde_json::from_slice(&*task.body_bytes) {
                    Err(e) =>{
                        if task.verbose{
                            eprintln!("JSONPath 查询失败: {}", e);
                        };
                        *task.err_count.lock().await += 1;
                        *task.api_err_count.lock().await += 1;
                        assertion_failed = true;
                        task.assert_errors.lock().await.increment(
                            String::from(task.endpoint.lock().await.url.clone()),
                            format!("{:?}-JSONPath查询失败:{:?}", task.api_name, e)).await;
                        None
                    }
                    Ok(val) => {
                        Some(val)
                    }
                };
                // 多断言
                for assert_option in &task.assert_options {
                    if task.body_bytes.len() == 0{
                        eprintln!("无法获取到结构体，不进行断言");
                        break
                    }
                    // 通过jsonpath提取数据
                    if let Some(json_val) = json_value.clone(){
                        match select(&json_val, &*assert_option.jsonpath) {
                            Ok(results) => {
                                if results.is_empty(){
                                    if task.verbose{
                                        eprintln!("没有匹配到任何结果");
                                    }
                                    *task.err_count.lock().await += 1;
                                    *task.api_err_count.lock().await += 1;
                                    task.assert_errors.lock().await.increment(
                                        String::from(task.endpoint.lock().await.url.clone()),
                                        format!("{:?}-JSONPath查询失败:{:?}", task.api_name, "没有匹配到任何结果")).await;
                                    assertion_failed = true;
                                    break;
                                }
                                if results.len() > 1{
                                    if task.verbose{
                                        eprintln!("匹配到多个值，无法进行断言");
                                    }
                                    *task.err_count.lock().await += 1;
                                    *task.api_err_count.lock().await += 1;
                                    task.assert_errors.lock().await.increment(
                                        String::from(task.endpoint.lock().await.url.clone()),
                                        format!("{:?}-JSONPath查询失败:{:?}", task.api_name, "匹配到多个值，无法进行断言")).await;
                                    assertion_failed = true;
                                    break;
                                }
                                // 取出匹配到的唯一值
                                if let Some(result) = results.get(0).map(|&v|v) {
                                    if *result != assert_option.reference_object{
                                        // 将失败情况加入到一个容器中
                                        task.assert_errors.
                                            lock().
                                            await.
                                            increment(
                                                String::from(task.endpoint.lock().await.url.clone()),
                                                format!(
                                                    "{:?}-预期结果：{:?}, 实际结果：{:?}", task.api_name, assert_option.reference_object, result
                                                )
                                            ).await;
                                        if task.verbose{
                                            eprintln!("{:?}-预期结果：{:?}, 实际结果：{:?}",task.api_name ,assert_option.reference_object, result)
                                        }
                                        // 错误数据增加
                                        *task.err_count.lock().await += 1;
                                        *task.api_err_count.lock().await += 1;
                                        // 退出断言
                                        assertion_failed = true;
                                        break;
                                    }
                                }
                            },
                            Err(e) => {
                                eprintln!("JSONPath 查询失败: {}", e);
                                assertion_failed = true;
                                break;
                            },
                        }
                    };
                }
                if !assertion_failed{
                    // 正确统计+1
                    *task.successful_requests.lock().await += 1;
                    // api正确统计+1
                    *task.api_successful_requests.lock().await += 1;
                };
                // 回调完成信号
                if let Err(_) = task.completion_signal.send(()){
                    eprintln!("回调任务状态失败");
                };
            }
            else => {
                eprintln!("断言任务执行完成！");
                break;
            }
        }
    }
}
