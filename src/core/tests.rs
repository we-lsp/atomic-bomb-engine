#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::batch::batch;
    use crate::models::api_endpoint::ApiEndpoint;
    use crate::models::assert_option::AssertOption;
    use crate::models::setup::JsonpathExtract;
    use crate::models::setup::SetupApiEndpoint;
    use crate::models::step_option::StepOption;
    use core::option::Option;
    use serde_json::{json, Value};

    #[tokio::test]
    async fn test_batch() {
        let mut assert_vec: Vec<AssertOption> = Vec::new();
        assert_vec.push(AssertOption {
            jsonpath: "$.code".to_string(),
            reference_object: Value::from(2000000),
        });
        let mut endpoints: Vec<ApiEndpoint> = Vec::new();

        // endpoints.push(ApiEndpoint {
        //     name: "有断言".to_string(),
        //     url: "https://ooooo.run/api/short/v1/getJumpCount/{{test-code}}".to_string(),
        //     method: "GET".to_string(),
        //     weight: 1,
        //     json: None,
        //     form_data: None,
        //     headers: None,
        //     cookies: None,
        //     assert_options: Some(assert_vec.clone()),
        //     // think_time_option: Some(ThinkTime {
        //     //     min_millis: 300,
        //     //     max_millis: 500,
        //     // }),
        //     think_time_option: None,
        //     setup_options: None,
        // });
        // //
        // endpoints.push(ApiEndpoint {
        //     name: "无断言".to_string(),
        //     url: "https://ooooo.run/api/short/v1/getJumpCount".to_string(),
        //     method: "POST".to_string(),
        //     weight: 3,
        //     json: None,
        //     form_data: None,
        //     headers: None,
        //     cookies: None,
        //     assert_options: None,
        //     think_time_option: None,
        //     setup_options: None,
        // });
        //
        endpoints.push(ApiEndpoint {
            name: "test-1".to_string(),
            url: "http://127.0.0.1:8080/direct".to_string(),
            method: "POST".to_string(),
            weight: 1,
            json: Some(json!({"name": "test","number": 10086})),
            headers: None,
            cookies: None,
            form_data: None,
            assert_options: Option::from(assert_vec),
            think_time_option: None,
            setup_options: None,
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
            headers: None,
            cookies: None,
            jsonpath_extract: Some(jsonpath_extracts),
        });
        match batch(
            10,
            30,
            10,
            true,
            true,
            true,
            endpoints,
            Option::from(StepOption {
                increase_step: 5,
                increase_interval: 2,
            }),
            None,
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
