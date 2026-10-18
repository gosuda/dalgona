//! Recorded `POST api/show` replies, shared by the probe and catalog tests.
//!
//! Live `https://ollama.com/api/show` replies recorded 2026-10-18 (the endpoint
//! answers without a key), trimmed to the members the decoder reads plus the
//! neighbours it must ignore, and replies of a local v0.40.6 server.
pub(crate) const GPT_OSS: &str = r#"{"capabilities":["completion","tools","thinking"],"details":{"parent_model":"gpt-oss:120b","format":"","family":"gptoss","families":null,"parameter_size":"116829156672","quantization_level":"MXFP4"},"model_info":{"general.architecture":"gptoss","general.parameter_count":116829156672,"gptoss.context_length":131072,"gptoss.embedding_length":2880},"modified_at":"2025-08-05T00:00:00Z","thinking":{"values":["low","medium","high"],"default":"medium"}}"#;
pub(crate) const GLM_5_2: &str = r#"{"capabilities":["thinking","completion","tools"],"model_info":{"general.architecture":"glm5.2","glm5.2.context_length":1048576},"thinking":{"values":[false,"high","max"],"default":"high"}}"#;
pub(crate) const DEEPSEEK_V41: &str = r#"{"capabilities":["completion","thinking","tools","vision"],"model_info":{"general.architecture":"deepseek_v41","deepseek_v41.context_length":1048576},"thinking":{"values":[false,"low","high","max"],"default":"high"}}"#;
pub(crate) const BEAM: &str = r#"{"capabilities":["completion","thinking","tools"],"model_info":{"general.architecture":"beam","beam.context_length":1048576},"thinking":{"values":["low","medium","high","xhigh","max"],"default":"max"}}"#;
pub(crate) const GEMMA_4: &str = r#"{"capabilities":["completion","thinking","tools","vision"],"model_info":{"general.architecture":"gemma4","gemma4.context_length":262144},"thinking":{"values":[false,true],"default":false}}"#;
pub(crate) const MINIMAX_M2_7: &str = r#"{"capabilities":["completion","tools","thinking"],"model_info":{"general.architecture":"minimax-m2","minimax-m2.context_length":196608},"thinking":{"values":[true],"default":true}}"#;
pub(crate) const MISTRAL_LARGE_4: &str = r#"{"capabilities":["completion","thinking","tools","vision"],"model_info":{"general.architecture":"mistral4","mistral4.context_length":1048576}}"#;
pub(crate) const NEMOTRON_ULTRA: &str = r#"{"capabilities":["completion","thinking","tools"],"model_info":{"general.architecture":"",".context_length":262144},"thinking":{"values":[false,true],"default":true}}"#;
// A local v0.40.6 server, `qwen3:0.6b` and `gemma3:270m`.
pub(crate) const LOCAL_QWEN3: &str = r#"{"capabilities":["completion","tools","thinking"],"model_info":{"general.architecture":"qwen3","qwen3.context_length":40960},"thinking":{"values":[false,true],"default":true}}"#;
pub(crate) const LOCAL_GEMMA3: &str = r#"{"capabilities":["completion"],"model_info":{"general.architecture":"gemma3","gemma3.context_length":32768}}"#;

