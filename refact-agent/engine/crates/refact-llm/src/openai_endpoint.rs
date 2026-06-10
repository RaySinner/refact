use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use reqwest::header::AUTHORIZATION;
use reqwest::header::CONTENT_TYPE;
use serde_json::json;
use tracing::info;

use refact_chat_history::history_limit::CompressionStrength;
use refact_core::chat_types::SamplingParameters;
use refact_core::custom_error::MapErrToString;
use refact_core::llm_types::BaseModelRecord;

pub async fn forward_to_openai_style_endpoint(
    model_rec: &BaseModelRecord,
    prompt: &str,
    client: &reqwest::Client,
    sampling_parameters: &SamplingParameters,
) -> Result<serde_json::Value, String> {
    if model_rec.endpoint.is_empty() {
        return Err(format!("No endpoint configured for {}", model_rec.id));
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str("application/json")
            .map_err(|e| format!("invalid content-type header: {}", e))?,
    );
    if !model_rec.api_key.is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", model_rec.api_key))
                .map_err(|e| format!("invalid api_key for authorization header: {}", e))?,
        );
    }
    let mut data = json!({
        "model": model_rec.name.clone(),
        "stream": false,
        "prompt": prompt,
        "echo": false,
    });
    if !sampling_parameters.stop.is_empty() {
        data["stop"] = serde_json::Value::from(sampling_parameters.stop.clone());
    };
    if let Some(n) = sampling_parameters.n {
        data["n"] = serde_json::Value::from(n);
    }
    if let Some(reasoning_effort) = sampling_parameters.reasoning_effort.clone() {
        data["reasoning_effort"] = serde_json::Value::String(reasoning_effort.to_string());
    } else if let Some(thinking) = sampling_parameters.thinking.clone() {
        data["thinking"] = thinking.clone();
    } else if let Some(enable_thinking) = sampling_parameters.enable_thinking {
        data["enable_thinking"] = serde_json::Value::Bool(enable_thinking);
        data["temperature"] = serde_json::Value::from(sampling_parameters.temperature);
    } else if let Some(temperature) = sampling_parameters.temperature {
        data["temperature"] = serde_json::Value::from(temperature);
    }
    data["max_completion_tokens"] = serde_json::Value::from(sampling_parameters.max_new_tokens);
    info!(
        "Request: model={}, reasoning_effort={}, T={}, n={}, stream=false",
        model_rec.name,
        sampling_parameters
            .reasoning_effort
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string()),
        sampling_parameters
            .temperature
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string()),
        sampling_parameters
            .n
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string())
    );
    let req = client
        .post(&model_rec.endpoint)
        .headers(headers)
        .body(data.to_string())
        .send()
        .await;
    let resp = req.map_err_to_string()?;
    let status_code = resp.status().as_u16();
    let response_txt = resp
        .text()
        .await
        .map_err(|e| format!("reading from socket {}: {}", model_rec.endpoint, e))?;
    if status_code != 200 && status_code != 400 {
        return Err(format!(
            "{} status={} text {}",
            model_rec.endpoint, status_code, response_txt
        ));
    }
    if status_code != 200 {
        tracing::info!(
            "forward_to_openai_style_endpoint: {} {}\n{}",
            model_rec.endpoint,
            status_code,
            response_txt
        );
    }
    let parsed_json: serde_json::Value = match serde_json::from_str(&response_txt) {
        Ok(json) => json,
        Err(e) => {
            return Err(format!(
                "Failed to parse JSON response: {}\n{}",
                e, response_txt
            ))
        }
    };
    Ok(parsed_json)
}

pub async fn forward_to_openai_style_endpoint_streaming(
    model_rec: &BaseModelRecord,
    prompt: &str,
    client: &reqwest::Client,
    sampling_parameters: &SamplingParameters,
) -> Result<reqwest::Response, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str("application/json")
            .map_err(|e| format!("invalid content-type header: {}", e))?,
    );
    if !model_rec.api_key.is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", model_rec.api_key))
                .map_err(|e| format!("invalid api_key for authorization header: {}", e))?,
        );
    }

    let mut data = json!({
        "model": model_rec.name,
        "stream": true,
        "stream_options": {"include_usage": true},
        "prompt": prompt,
    });

    if !sampling_parameters.stop.is_empty() {
        data["stop"] = serde_json::Value::from(sampling_parameters.stop.clone());
    };
    if let Some(n) = sampling_parameters.n {
        data["n"] = serde_json::Value::from(n);
    }

    if let Some(reasoning_effort) = sampling_parameters.reasoning_effort.clone() {
        data["reasoning_effort"] = serde_json::Value::String(reasoning_effort.to_string());
    } else if let Some(thinking) = sampling_parameters.thinking.clone() {
        data["thinking"] = thinking.clone();
    } else if let Some(enable_thinking) = sampling_parameters.enable_thinking {
        data["enable_thinking"] = serde_json::Value::Bool(enable_thinking);
        data["temperature"] = serde_json::Value::from(sampling_parameters.temperature);
    } else if let Some(temperature) = sampling_parameters.temperature {
        data["temperature"] = serde_json::Value::from(temperature);
    }
    data["max_completion_tokens"] = serde_json::Value::from(sampling_parameters.max_new_tokens);

    info!(
        "Request: model={}, reasoning_effort={}, T={}, n={}, stream=true",
        model_rec.name,
        sampling_parameters
            .reasoning_effort
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string()),
        sampling_parameters
            .temperature
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string()),
        sampling_parameters
            .n
            .clone()
            .map(|x| x.to_string())
            .unwrap_or("none".to_string())
    );

    if model_rec.endpoint.is_empty() {
        return Err(format!("No endpoint configured for {}", model_rec.id));
    }
    let response = client
        .post(&model_rec.endpoint)
        .headers(headers)
        .body(data.to_string())
        .send()
        .await
        .map_err(|e| format!("can't stream from {}: {}", model_rec.endpoint, e))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {} from {}: {}",
            status, model_rec.endpoint, text
        ));
    }
    Ok(response)
}

pub fn try_get_compression_from_prompt(_prompt: &str) -> serde_json::Value {
    json!(CompressionStrength::Absent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_endpoint_returns_error() {
        let model_rec = BaseModelRecord {
            id: "test-model".to_string(),
            name: "test".to_string(),
            ..Default::default()
        };
        let client = reqwest::Client::new();
        let sampling_parameters = SamplingParameters::default();

        let result =
            forward_to_openai_style_endpoint(&model_rec, "prompt", &client, &sampling_parameters)
                .await;

        assert_eq!(result.unwrap_err(), "No endpoint configured for test-model");
    }

    #[test]
    fn compression_from_prompt_returns_absent_json_shape() {
        assert_eq!(try_get_compression_from_prompt("prompt"), json!("absent"));
    }
}
