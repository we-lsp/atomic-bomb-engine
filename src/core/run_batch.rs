use tokio::sync::mpsc;

use crate::core::batch;
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::result::BatchResult;
use crate::models::setup::SetupApiEndpoint;
use crate::models::step_option::StepOption;

pub async fn run_batch(
    test_duration_secs: u64,
    concurrent_requests: usize,
    timeout_secs: u64,
    cookie_store_enable: bool,
    verbose: bool,
    should_prevent: bool,
    api_endpoints: Vec<ApiEndpoint>,
    step_option: Option<StepOption>,
    setup_options: Option<Vec<SetupApiEndpoint>>,
    assert_channel_buffer_size: usize,
) -> mpsc::Receiver<Option<BatchResult>> {
    let (sender, receiver) = mpsc::channel(1024);
    tokio::spawn(async move {
        let res = batch::batch(
            sender.clone(),
            test_duration_secs,
            concurrent_requests,
            timeout_secs,
            cookie_store_enable,
            verbose,
            should_prevent,
            api_endpoints,
            step_option,
            setup_options,
            assert_channel_buffer_size,
        )
        .await;
        match res {
            Ok(r) => {
                match sender.send(Some(r)).await {
                    Ok(_) => {
                        println!("压测结束");
                    }
                    Err(_) => {
                        eprintln!("压测结束，但是发送结果失败");
                    }
                };
                match sender.send(None).await {
                    Ok(_) => {
                        println!("发送结束信号");
                    }
                    Err(_) => {
                        eprintln!("发送结束信号失败");
                    }
                };
            }
            Err(e) => {
                eprintln!("Error: {:?}", e.to_string());
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use core::option::Option;

    use crate::core::batch::batch;
    use serde_json::{json, Value};

    use crate::models::api_endpoint::ApiEndpoint;
    use crate::models::assert_option::AssertOption;
    use crate::models::setup::JsonpathExtract;
    use crate::models::setup::SetupApiEndpoint;
    use crate::models::step_option::StepOption;

    use super::*;

    #[tokio::test]
    async fn test_run_batch() {
        let mut assert_vec: Vec<AssertOption> = Vec::new();
        assert_vec.push(AssertOption {
            jsonpath: "$.code".to_string(),
            reference_object: Value::from(2000000),
        });
        let mut endpoints: Vec<ApiEndpoint> = Vec::new();
        endpoints.push(ApiEndpoint {
            name: "test-1".to_string(),
            url: "http://127.0.0.1:8080/ran_sleep".to_string(),
            method: "POST".to_string(),
            weight: 100,
            json: Some(json!({"name": "test","number": 10086})),
            headers: None,
            cookies: None,
            form_data: None,
            assert_options: Option::from(assert_vec),
            think_time_option: None,
            setup_options: None,
            multipart_options: None,
        });
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
            multipart_options: None,
            headers: None,
            cookies: None,
            jsonpath_extract: Some(jsonpath_extracts),
        });

        let (tx, _rx) = mpsc::channel(1024);
        let tx_clone = tx.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let res = batch::batch(
                tx_clone,
                10,
                5,
                10,
                true,
                false,
                true,
                endpoints,
                Option::from(StepOption {
                    increase_step: 1,
                    increase_interval: 2,
                }),
                None,
                4096,
            )
            .await;
            match res {
                Ok(r) => {
                    match tx.send(Some(r)) {
                        Ok(_) => {
                            println!("压测结束");
                        }
                        Err(_) => {
                            eprintln!("压测结束，但是发送结果失败");
                        }
                    };
                    match tx.send(None) {
                        Ok(_) => {
                            println!("发送结束信号");
                        }
                        Err(_) => {
                            eprintln!("发送结束信号失败");
                        }
                    };
                }
                Err(e) => {
                    eprintln!("Error: {:?}", e.to_string());
                }
            }
        });
        while let Ok(Some(res)) = rx.recv().await {
            println!("{:?}", res)
        }
    }
}
