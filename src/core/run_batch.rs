use tokio::sync::mpsc;

use crate::core::batch;
use crate::models::api_endpoint::ApiEndpoint;
use crate::models::result::BatchResult;
use crate::models::setup::SetupApiEndpoint;
use crate::models::step_option::StepOption;
use futures::stream::{BoxStream, StreamExt};

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
) -> BoxStream<'static, Result<Option<BatchResult>, anyhow::Error>> {
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
                if let Err(_) = sender.send(Some(r)).await {
                    eprintln!("压测结束，但是发送结果失败");
                }
                if let Err(_) = sender.send(None).await {
                    eprintln!("发送结束信号失败");
                }
            }
            Err(e) => {
                eprintln!("Error: {:?}", e.to_string());
            }
        }
    });

    let stream = futures::stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Some(Some(batch_result)) => Some((Ok(Some(batch_result)), receiver)),
            Some(None) => Some((Ok(None), receiver)),
            None => None,
        }
    });

    stream.boxed()
}

#[cfg(test)]
mod tests {
    use core::option::Option;
    use futures::pin_mut;

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

        let mut batch_stream = run_batch(
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
        loop {
            if let Some(result) = batch_stream.next().await {
                match result {
                    Ok(Some(batch_result)) => {
                        println!("Received batch result: {:?}", batch_result);
                    }
                    Ok(None) => {
                        println!("No more results.");
                        break;
                    }
                    Err(e) => {
                        println!("Error: {:?}", e);
                    }
                }
            }
        }
    }
}
