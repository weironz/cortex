//! 配置。
//!
//! 单一来源：环境变量（开发期由 `.env` 注入）。
//! 供应商相关的 `GOOSE_*` 变量在此收编，用户只面对 `CORTEX_*` / 标准的
//! `*_API_KEY`，不暴露取件来源的命名。

use std::env;

use crate::error::{CortexError, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub s3: S3Config,
    pub llm: LlmConfig,
    /// 本机标识，写入每一行数据用于审计与同步溯源。
    pub device_id: String,
}

#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 供应商标识：deepseek / anthropic / openai / ollama …
    pub provider: String,
    /// 主对话模型
    pub model: String,
    /// 抽取、摘要等后台任务用的廉价模型
    pub cheap_model: String,
}

fn required(key: &str) -> Result<String> {
    env::var(key).map_err(|_| CortexError::Config(format!("缺少环境变量 {key}")))
}

fn optional(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    /// 从环境变量加载。开发期先由调用方 `dotenvy::dotenv()` 注入 `.env`。
    pub fn from_env() -> Result<Self> {
        let provider = optional("CORTEX_LLM_PROVIDER", "deepseek");

        Ok(Self {
            database_url: required("DATABASE_URL")?,
            bind: optional("CORTEXD_BIND", "127.0.0.1:8080"),
            s3: S3Config {
                endpoint: optional("S3_ENDPOINT", "http://localhost:9010"),
                bucket: optional("S3_BUCKET", "cortex-blobs"),
                region: optional("S3_REGION", "us-east-1"),
                access_key: optional("RUSTFS_ACCESS_KEY", "cortexadmin"),
                secret_key: optional("RUSTFS_SECRET_KEY", "cortex_dev_only"),
            },
            llm: LlmConfig {
                model: optional("CORTEX_LLM_MODEL", default_model(&provider)),
                cheap_model: optional("CORTEX_LLM_CHEAP_MODEL", default_cheap_model(&provider)),
                provider,
            },
            device_id: optional("CORTEX_DEVICE_ID", "dev-local"),
        })
    }
}

fn default_model(provider: &str) -> &'static str {
    match provider {
        "deepseek" => "deepseek-v4-pro",
        "anthropic" => "claude-opus-5",
        "openai" => "gpt-5.5",
        _ => "deepseek-v4-pro",
    }
}

fn default_cheap_model(provider: &str) -> &'static str {
    match provider {
        "deepseek" => "deepseek-v4-flash",
        "anthropic" => "claude-haiku-4-5-20251001",
        "openai" => "gpt-5-mini",
        _ => "deepseek-v4-flash",
    }
}
