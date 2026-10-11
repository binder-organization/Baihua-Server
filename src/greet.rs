use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

pub async fn greet() -> impl IntoResponse {
    #[derive(Serialize)]
    struct GreetResponse {
        server_version: String,
        api_version: String,
        message: String,
    }

    // Modifications are required when the version is upgraded.
    let response = GreetResponse {
        server_version: "0.1.5".to_string(),
        api_version: "v1".to_string(),
        message: "Hello Baihua.".to_string(),
    };

    (StatusCode::OK, Json(response))
}
