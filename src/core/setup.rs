use crate::models::setup::SetupApiEndpoint;
use anyhow::Error;
use futures::StreamExt;
use handlebars::Handlebars;
use jsonpath_lib::select;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::str::FromStr;

pub async fn start_setup(
    setup_options: Vec<SetupApiEndpoint>,
    extract_b_tree_map: BTreeMap<String, Value>,
    client: Client,
) -> Result<Option<BTreeMap<String, Value>>, Error> {
    let mut extract_map: BTreeMap<String, Value> = BTreeMap::new();
    extract_map.extend(extract_b_tree_map);
    for option in setup_options {
        // 构建请求方式
        let method = Method::from_str(&option.method.to_uppercase())
            .map_err(|_| Error::msg("构建请求方法失败"))?;
        // 构建请求
        let mut request = client.request(method, option.url);
        // 构建请求头
        let mut headers = HeaderMap::new();
        if let Some(headers_map) = option.headers {
            headers.extend(headers_map.iter().map(|(k, v)| {
                let header_name = k.parse::<HeaderName>().expect("无效的header名称");
                let header_value = v.parse::<HeaderValue>().expect("无效的header值");
                (header_name, header_value)
            }));
        }
        // 构建cookies
        if let Some(ref c) = option.cookies {
            match HeaderValue::from_str(c) {
                Ok(h) => {
                    headers.insert(COOKIE, h);
                }
                Err(e) => return Err(Error::msg(format!("设置cookie失败:{:?}", e))),
            }
        }
        // 将header替换
        headers.iter_mut().for_each(|(_key, value)| {
            let handlebars = Handlebars::new();
            let val_str = match value.to_str(){
                Ok(v) => {v}
                Err(e) => {
                    eprintln!("设置header失败::{:?}", e.to_string());
                    return
                }
            };
            let new_val =
                match handlebars.render_template(val_str, &json!(extract_map)) {
                    Ok(v) => {
                        let header_value = v.parse::<HeaderValue>().expect("无效的header值");
                        header_value
                    }
                    Err(e) => {
                        eprintln!("{:?}", e);
                        value.clone()
                    }
                };
            *value = new_val;
        });
        request = request.headers(headers);
        // 构建json请求
        if let Some(json_value) = option.json {
            let handlebars = Handlebars::new();
            let json_string =
                match handlebars.render_template(&*json_value.to_string(), &json!(extract_map)) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("{:?}", e);
                        json_value.to_string()
                    }
                };
            // println!("{:?}",json_string);
            let json_val = match Value::from_str(&*json_string) {
                Ok(val) => val,
                Err(e) => {
                    return Err(Error::msg(format!(
                        "转换json失败:{:?},原始json: {:?}",
                        e,
                        json_string.to_string()
                    )))
                }
            };
            request = request.json(&json_val);
        }
        // 构建form表单
        if let Some(mut form_data) = option.form_data {
            form_data.iter_mut().for_each(|(_key, value)| {
                let handlebars = Handlebars::new();
                let new_val = match handlebars.render_template(value, &json!(extract_map)) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{:?}", e);
                        value.to_string()
                    }
                };
                *value = new_val;
            });
            request = request.form(&form_data);
        };
        // 发送请求
        match request.send().await {
            Ok(response) => {
                // 需要通过jsonpath提取数据
                if let Some(json_path_vec) = option.jsonpath_extract {
                    // 响应流
                    let mut stream = response.bytes_stream();
                    // 响应体
                    let mut body_bytes = Vec::new();
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(chunk) => {
                                // 获取当前的chunk
                                body_bytes.extend_from_slice(&chunk);
                            }
                            Err(e) => {
                                { Err(Error::msg(format!("获取响应流失败:{:?}", e))) }?
                            }
                        };
                    }
                    for jsonpath_obj in json_path_vec {
                        let jsonpath = jsonpath_obj.jsonpath;
                        let key = jsonpath_obj.key;
                        // 将响应转换为json
                        let json_value: Value = match serde_json::from_slice(&*body_bytes) {
                            Err(e) => {
                                let err_msg = match String::from_utf8(body_bytes){
                                    Ok(msg) => {msg}
                                    Err(e) => {
                                        e.to_string()
                                    }
                                };
                                return Err(Error::msg(format!(
                                    "转换json失败:{:?}, 原始json: {:?}",
                                    e, err_msg
                                )));
                            }
                            Ok(val) => val,
                        };
                        // 通过jsonpath提取数据
                        match select(&json_value, &jsonpath) {
                            Ok(results) => {
                                if results.is_empty() {
                                    return Err(Error::msg("初始化失败::jsonpath没有匹配到任何值"));
                                }
                                if results.len() > 1 {
                                    return Err(Error::msg("初始化失败::jsonpath匹配的不是唯一值"));
                                }
                                // 取出匹配到的唯一值
                                if let Some(result) = results.get(0).map(|&v| v) {
                                    // 检查key是否冲突
                                    if extract_map.contains_key(&key) {
                                        return Err(Error::msg(format!(
                                            "初始化失败::key {:?}冲突",
                                            key
                                        )));
                                    }
                                    // 将key设置到字典中
                                    extract_map.insert(key.clone(), result.clone());
                                }
                            }
                            Err(e) => {
                                return Err(Error::msg(format!("jsonpath提取数据失败::{:?}", e)))
                            }
                        }
                    }
                }
            }
            Err(err) => {
                return Err(Error::msg(format!("初始化失败:{:?}", err)));
            }
        }
    }
    match extract_map.len() > 0 {
        true => Ok(Some(extract_map)),
        false => Ok(None),
    }
}
