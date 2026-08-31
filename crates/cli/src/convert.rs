//! Versioned NDJSON framing for the shared, source-neutral converter contract.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mathpreview_core::converter::{
    self, BuiltinConverter, ConversionRequest, ConvertedDependency, ConvertedDocument,
    DependencyKind, MAX_CONVERSION_OVERRIDE_BYTES, MAX_CONVERSION_OVERRIDE_FILES,
};
use mathpreview_core::{Engine, HtmlOptions, MathJaxEngine};
use serde::{Deserialize, Serialize};

pub const CONVERSION_PROTOCOL_V1: &str = "mathpreview.converter/v1";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 4 * 1024;
const MAX_LAYER_FILES: usize = 128;

#[derive(Debug, Clone, Default)]
pub struct ConvertCommandOptions {
    pub config_files: Vec<PathBuf>,
    pub macro_files: Vec<PathBuf>,
    pub mathjax_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionRequestV1 {
    pub protocol: String,
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub source: Option<String>,
    /// Unsaved included, preamble, or bibliography files keyed by source path.
    #[serde(default)]
    pub file_overrides: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub converter: Option<String>,
    #[serde(default)]
    pub options: ConversionOptionsV1,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversionOptionsV1 {
    pub title: Option<String>,
    /// URL or relative path a consumer should load MathJax from.
    pub mathjax_url: Option<String>,
    pub config_files: Vec<PathBuf>,
    pub macro_files: Vec<PathBuf>,
    pub local_asset_base: Option<String>,
    pub tikz_asset_base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionResponseV1 {
    pub protocol: String,
    pub id: Option<String>,
    #[serde(flatten)]
    pub outcome: ConversionOutcomeV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ConversionOutcomeV1 {
    Ok { result: Box<ConvertedDocument> },
    Error { error: ConversionErrorV1 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionErrorV1 {
    pub code: ConversionErrorCodeV1,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConversionErrorCodeV1 {
    InvalidJson,
    InvalidRequest,
    RequestTooLarge,
    UnsupportedProtocol,
    UnsupportedConverter,
    ConfigurationFailed,
    ConversionFailed,
}

pub fn run_ndjson<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    command_options: &ConvertCommandOptions,
) -> Result<()> {
    run_ndjson_with_limit(reader, writer, command_options, MAX_REQUEST_BYTES)
}

fn run_ndjson_with_limit<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    command_options: &ConvertCommandOptions,
    max_request_bytes: usize,
) -> Result<()> {
    while let Some(line) = read_bounded_line(&mut reader, max_request_bytes)? {
        let response = match line {
            BoundedLine::Line(line) if line.iter().all(u8::is_ascii_whitespace) => continue,
            BoundedLine::Line(line) => handle_request_line(&line, command_options),
            BoundedLine::TooLarge => error_response(
                None,
                ConversionErrorCodeV1::RequestTooLarge,
                format!("request exceeds the {max_request_bytes}-byte limit"),
            ),
        };
        write_response(&mut writer, &response)?;
    }
    Ok(())
}

fn write_response<W: Write>(writer: &mut W, response: &ConversionResponseV1) -> Result<()> {
    let encoded = match serde_json::to_vec(response) {
        Ok(encoded) => encoded,
        Err(error) => {
            let fallback = error_response(
                response.id.clone(),
                ConversionErrorCodeV1::ConversionFailed,
                format!("conversion response could not be encoded as JSON: {error}"),
            );
            serde_json::to_vec(&fallback).context("encoding conversion failure response")?
        }
    };
    writer
        .write_all(&encoded)
        .context("writing conversion response")?;
    writer
        .write_all(b"\n")
        .context("terminating conversion response")?;
    writer.flush().context("flushing conversion response")?;
    Ok(())
}

pub fn run_stdio(command_options: &ConvertCommandOptions) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_ndjson(stdin.lock(), stdout.lock(), command_options)
}

fn handle_request_line(
    line: &[u8],
    command_options: &ConvertCommandOptions,
) -> ConversionResponseV1 {
    let value: serde_json::Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(error) => {
            return error_response(
                None,
                ConversionErrorCodeV1::InvalidJson,
                format!("invalid JSON: {error}"),
            )
        }
    };
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| id.len() <= MAX_REQUEST_ID_BYTES)
        .map(str::to_string);
    let request: ConversionRequestV1 = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                id,
                ConversionErrorCodeV1::InvalidRequest,
                format!("invalid request: {error}"),
            )
        }
    };
    if request.id.len() > MAX_REQUEST_ID_BYTES {
        return error_response(
            None,
            ConversionErrorCodeV1::InvalidRequest,
            format!("request id exceeds the {MAX_REQUEST_ID_BYTES}-byte limit"),
        );
    }
    if request.protocol != CONVERSION_PROTOCOL_V1 {
        return error_response(
            Some(request.id),
            ConversionErrorCodeV1::UnsupportedProtocol,
            format!(
                "unsupported protocol {:?}; expected {:?}",
                request.protocol, CONVERSION_PROTOCOL_V1
            ),
        );
    }

    let id = request.id.clone();
    match convert_request(request, command_options) {
        Ok(result) => ConversionResponseV1 {
            protocol: CONVERSION_PROTOCOL_V1.to_string(),
            id: Some(id),
            outcome: ConversionOutcomeV1::Ok {
                result: Box::new(result),
            },
        },
        Err(error) => ConversionResponseV1 {
            protocol: CONVERSION_PROTOCOL_V1.to_string(),
            id: Some(id),
            outcome: ConversionOutcomeV1::Error { error },
        },
    }
}

fn convert_request(
    request: ConversionRequestV1,
    command_options: &ConvertCommandOptions,
) -> std::result::Result<ConvertedDocument, ConversionErrorV1> {
    validate_wire_request(&request, command_options)?;
    let selected_converter = select_converter(&request)?;
    let input_dir = request.path.parent().unwrap_or_else(|| Path::new("."));
    let mut extra_configs = command_options.config_files.clone();
    extra_configs.extend(request.options.config_files.iter().cloned());
    let config_files = mathpreview_core::discover_config_files(input_dir, &extra_configs);
    let (resolved, _) =
        mathpreview_core::load_and_merge_config(&config_files).map_err(|error| {
            protocol_error(
                ConversionErrorCodeV1::ConfigurationFailed,
                format!("loading configuration: {error:#}"),
            )
        })?;

    let mut extra_macros = command_options.macro_files.clone();
    extra_macros.extend(request.options.macro_files.iter().cloned());
    let macro_files = mathpreview_core::discover_macro_overrides(input_dir, &extra_macros);
    let title = request.options.title.clone().unwrap_or_else(|| {
        request
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("mathpreview")
            .to_string()
    });
    let mut opts = HtmlOptions {
        title,
        macro_overrides: macro_files.clone(),
        viewer_config: resolved.viewer,
        markdown_config: resolved.markdown,
        text_macros: resolved.text_macros,
        local_asset_base: request.options.local_asset_base,
        tikz_asset_base: request.options.tikz_asset_base,
        ..HtmlOptions::default()
    };
    if let Some(url) = request
        .options
        .mathjax_url
        .or_else(|| command_options.mathjax_url.clone())
    {
        opts.engine = Engine::MathJax(MathJaxEngine::new(url));
    }

    let request_path = request.path.clone();
    let mut core_request = match request.source {
        Some(source) => ConversionRequest::from_source(&request_path, source),
        None => ConversionRequest::from_path(&request_path),
    }
    .with_file_overrides(request.file_overrides);
    core_request.additional_dependencies.extend(
        config_files
            .into_iter()
            .map(|path| ConvertedDependency::new(path, DependencyKind::config())),
    );
    core_request.additional_dependencies.extend(
        macro_files
            .into_iter()
            .map(|path| ConvertedDependency::new(path, DependencyKind::macro_file())),
    );
    core_request.validate().map_err(|error| {
        protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            format!("invalid conversion request: {error:#}"),
        )
    })?;

    selected_converter
        .convert(core_request, &opts)
        .map_err(|error| {
            protocol_error(
                ConversionErrorCodeV1::ConversionFailed,
                format!("converting {}: {error:#}", request_path.display()),
            )
        })
}

fn validate_wire_request(
    request: &ConversionRequestV1,
    command_options: &ConvertCommandOptions,
) -> std::result::Result<(), ConversionErrorV1> {
    if request.path.as_os_str().is_empty() {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            "path must not be empty".to_string(),
        ));
    }
    if request.file_overrides.len() > MAX_CONVERSION_OVERRIDE_FILES {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            format!("at most {MAX_CONVERSION_OVERRIDE_FILES} file overrides may be supplied"),
        ));
    }
    let override_bytes = request.file_overrides.values().try_fold(
        request.source.as_ref().map_or(0usize, String::len),
        |total, source| total.checked_add(source.len()),
    );
    let Some(override_bytes) = override_bytes else {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            "file override byte count overflow".to_string(),
        ));
    };
    let wire_override_limit = MAX_CONVERSION_OVERRIDE_BYTES.min(MAX_REQUEST_BYTES);
    if override_bytes > wire_override_limit {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            format!(
                "buffer sources contain {override_bytes} bytes; maximum is {wire_override_limit}"
            ),
        ));
    }
    let config_count = command_options.config_files.len() + request.options.config_files.len();
    if config_count > MAX_LAYER_FILES {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            format!("at most {MAX_LAYER_FILES} config files may be supplied"),
        ));
    }
    let macro_count = command_options.macro_files.len() + request.options.macro_files.len();
    if macro_count > MAX_LAYER_FILES {
        return Err(protocol_error(
            ConversionErrorCodeV1::InvalidRequest,
            format!("at most {MAX_LAYER_FILES} macro files may be supplied"),
        ));
    }
    Ok(())
}

fn select_converter(
    request: &ConversionRequestV1,
) -> std::result::Result<BuiltinConverter, ConversionErrorV1> {
    match request.converter.as_deref().unwrap_or("auto") {
        "auto" => Ok(converter::converter_for_path(&request.path)),
        "latex" => Ok(BuiltinConverter::Latex),
        "markdown" => Ok(BuiltinConverter::Markdown),
        converter => Err(protocol_error(
            ConversionErrorCodeV1::UnsupportedConverter,
            format!("unknown converter {converter:?}; expected auto, latex, or markdown"),
        )),
    }
}

fn protocol_error(code: ConversionErrorCodeV1, message: String) -> ConversionErrorV1 {
    ConversionErrorV1 { code, message }
}

fn error_response(
    id: Option<String>,
    code: ConversionErrorCodeV1,
    message: String,
) -> ConversionResponseV1 {
    ConversionResponseV1 {
        protocol: CONVERSION_PROTOCOL_V1.to_string(),
        id,
        outcome: ConversionOutcomeV1::Error {
            error: protocol_error(code, message),
        },
    }
}

enum BoundedLine {
    Line(Vec<u8>),
    TooLarge,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_request_bytes: usize,
) -> io::Result<Option<BoundedLine>> {
    let mut line = Vec::new();
    let read = {
        let mut limited = reader
            .by_ref()
            .take((max_request_bytes.saturating_add(2)) as u64);
        limited.read_until(b'\n', &mut line)?
    };
    if read == 0 {
        return Ok(None);
    }
    let ended = line.last() == Some(&b'\n');
    if ended {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    if line.len() <= max_request_bytes {
        return Ok(Some(BoundedLine::Line(line)));
    }
    if !ended {
        discard_through_newline(reader)?;
    }
    Ok(Some(BoundedLine::TooLarge))
}

fn discard_through_newline<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            return Ok(());
        }
        let length = buffer.len();
        reader.consume(length);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview-convert-{name}-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn responses(bytes: &[u8]) -> Vec<serde_json::Value> {
        std::str::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn stream_continues_after_malformed_request() {
        let input = concat!(
            "not json\n",
            r#"{"protocol":"wrong","id":"bad-version","path":"notes.md"}"#,
            "\n",
            r##"{"protocol":"mathpreview.converter/v1","id":"ok","path":"notes.md","source":"# Hello"}"##,
            "\n",
        );
        let mut output = Vec::new();
        run_ndjson(
            input.as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let messages = responses(&output);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["error"]["code"], "invalid-json");
        assert_eq!(messages[1]["error"]["code"], "unsupported-protocol");
        assert_eq!(messages[2]["status"], "ok");
        assert_eq!(messages[2]["result"]["converter"]["format"], "markdown");
    }

    #[cfg(unix)]
    #[test]
    fn response_encoding_failure_is_framed_and_does_not_end_the_stream() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let mut invalid = handle_request_line(
            br##"{"protocol":"mathpreview.converter/v1","id":"invalid-path","path":"notes.md","source":"# Notes"}"##,
            &ConvertCommandOptions::default(),
        );
        let ConversionOutcomeV1::Ok { result } = &mut invalid.outcome else {
            panic!("fixture conversion should succeed");
        };
        result.dependencies.push(ConvertedDependency::new(
            PathBuf::from(OsString::from_vec(vec![b'd', b'e', b'p', 0xff])),
            DependencyKind::include(),
        ));

        let valid = handle_request_line(
            br##"{"protocol":"mathpreview.converter/v1","id":"next","path":"notes.md","source":"Still alive"}"##,
            &ConvertCommandOptions::default(),
        );
        let mut output = Vec::new();
        write_response(&mut output, &invalid).unwrap();
        write_response(&mut output, &valid).unwrap();

        let messages = responses(&output);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["id"], "invalid-path");
        assert_eq!(messages[0]["error"]["code"], "conversion-failed");
        assert_eq!(messages[1]["id"], "next");
        assert_eq!(messages[1]["status"], "ok");
    }

    #[test]
    fn wire_version_tracks_core_contract() {
        assert_eq!(
            CONVERSION_PROTOCOL_V1,
            format!(
                "mathpreview.converter/v{}",
                mathpreview_core::converter::CONVERTER_API_VERSION
            )
        );
    }

    #[test]
    fn markdown_math_rows_are_exposed_on_the_wire() {
        let request = serde_json::json!({
            "protocol": CONVERSION_PROTOCOL_V1,
            "id": "markdown-rows",
            "path": "notes.md",
            "source": "$$\n\\begin{aligned}\n  a &= b \\\\\n    c &= d\n\\end{aligned}\n$$\n"
        });
        let mut output = Vec::new();
        run_ndjson(
            format!("{request}\n").as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let result = &responses(&output)[0]["result"];

        assert_eq!(result["converter"]["capabilities"]["math_row_sync"], true);
        assert_eq!(result["sync"]["math_rows"][0]["rows"][0]["start_line"], 3);
        assert_eq!(result["sync"]["math_rows"][0]["rows"][0]["start_col"], 3);
        assert_eq!(result["sync"]["math_rows"][0]["rows"][1]["start_line"], 4);
        assert_eq!(result["sync"]["math_rows"][0]["rows"][1]["start_col"], 5);
    }

    #[test]
    fn public_builders_construct_a_custom_converter_artifact() {
        let mut capabilities = mathpreview_core::ConverterCapabilities::default();
        capabilities.buffer_source = true;
        capabilities.block_patching = true;
        let metadata = mathpreview_core::ConverterMetadata::new("example", "example-markup")
            .with_extensions(["example"])
            .with_capabilities(capabilities);
        let block = mathpreview_core::RenderedBlock::new(
            "blk-1",
            "content-hash",
            "stable-hash",
            "<p>Example</p>",
        );
        let document = mathpreview_core::ConvertedDocument::new(
            metadata.clone(),
            "notes.example",
            "<p>Example</p>",
            mathpreview_core::RuntimeRequirements::new("Example"),
        )
        .with_blocks([block]);

        assert!(document.validate_against(&metadata).is_ok());
        assert_eq!(document.blocks.len(), 1);
    }

    #[test]
    fn response_reuses_neutral_diff_and_runtime_contract() {
        let input = concat!(
            r#"{"protocol":"mathpreview.converter/v1","id":"map","path":"paper.tex","source":"\\documentclass{article}\n\\newtheorem{theorem}{Theorem}\n\\newcommand{\\RR}{\\mathbb{R}}\n\\begin{document}\n\\begin{theorem}\nFirst paragraph.\n\nSecond paragraph with $\\RR$.\n\\end{theorem}\n\\end{document}"}"#,
            "\n",
        );
        let mut output = Vec::new();
        run_ndjson(
            input.as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let result = &responses(&output)[0]["result"];

        assert!(result.get("html").is_none());
        assert!(result.get("preamble").is_none());
        assert!(result.get("ast").is_none());
        let blocks = result["blocks"].as_array().unwrap();
        assert!(blocks.iter().all(|block| block.get("diff_hash").is_some()));
        let sub_blocks = blocks
            .iter()
            .find_map(|block| block["sub_blocks"].as_object())
            .expect("theorem exposes sub-block metadata");
        assert!(sub_blocks["children"]
            .as_array()
            .unwrap()
            .iter()
            .all(|child| child.get("diff_hash").is_some() && child.get("html").is_some()));
        assert!(result["sync"].get("by_label").is_none());
        assert_eq!(result["runtime"]["math"]["engine"], "mathjax");
        assert_eq!(
            result["runtime"]["math"]["script_url"],
            "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js"
        );
        assert!(result["runtime"]["math"]["macros"]
            .as_array()
            .unwrap()
            .iter()
            .any(|definition| definition["name"] == "RR"));
    }

    #[test]
    fn request_mathjax_url_overrides_process_default() {
        let input = concat!(
            r#"{"protocol":"mathpreview.converter/v1","id":"url","path":"notes.md","source":"$x$","options":{"mathjax_url":"/request/mathjax.js"}}"#,
            "\n",
        );
        let mut output = Vec::new();
        run_ndjson(
            input.as_bytes(),
            &mut output,
            &ConvertCommandOptions {
                mathjax_url: Some("/process/mathjax.js".to_string()),
                ..ConvertCommandOptions::default()
            },
        )
        .unwrap();
        let result = &responses(&output)[0]["result"];
        assert_eq!(
            result["runtime"]["math"]["script_url"],
            "/request/mathjax.js"
        );
    }

    #[test]
    fn process_mathjax_url_applies_when_request_omits_it() {
        let input = concat!(
            r#"{"protocol":"mathpreview.converter/v1","id":"url","path":"notes.md","source":"$x$"}"#,
            "\n",
        );
        let mut output = Vec::new();
        run_ndjson(
            input.as_bytes(),
            &mut output,
            &ConvertCommandOptions {
                mathjax_url: Some("/process/mathjax.js".to_string()),
                ..ConvertCommandOptions::default()
            },
        )
        .unwrap();
        let result = &responses(&output)[0]["result"];
        assert_eq!(
            result["runtime"]["math"]["script_url"],
            "/process/mathjax.js"
        );
    }

    #[test]
    fn config_and_macros_are_shared_dependencies() {
        let dir = temp_dir("layers");
        let config = dir.join("viewer.toml");
        let macros = dir.join("macros.tex");
        fs::write(&config, "[viewer]\nfont-size = 27\n").unwrap();
        fs::write(&macros, "\\newcommand{\\NN}{\\mathbb{N}}\n").unwrap();
        let request = serde_json::json!({
            "protocol": CONVERSION_PROTOCOL_V1,
            "id": "layers",
            "path": dir.join("notes.md"),
            "source": "$\\NN$",
            "options": { "config_files": [config], "macro_files": [macros] }
        });
        let mut output = Vec::new();
        run_ndjson(
            format!("{request}\n").as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let result = &responses(&output)[0]["result"];
        assert_eq!(result["runtime"]["viewer"]["font_size"], 27);
        let kinds: Vec<_> = result["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|dependency| dependency["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"config"));
        assert!(kinds.contains(&"macro"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsaved_tex_root_and_include_convert_together() {
        let dir = temp_dir("multi-buffer");
        let root = dir.join("main.tex");
        let definitions = dir.join("defs.tex");
        fs::write(
            &root,
            "\\documentclass{article}\n\\input{defs}\n\\begin{document}\nDisk root.\n\\end{document}\n",
        )
        .unwrap();
        fs::write(&definitions, "\\newcommand{\\ZZ}{\\mathbf{OLD}}\n").unwrap();
        let request = serde_json::json!({
            "protocol": CONVERSION_PROTOCOL_V1,
            "id": "buffers",
            "path": root,
            "source": "\\documentclass{article}\n\\input{defs}\n\\begin{document}\nBuffer root: $\\ZZ$.\n\\end{document}\n",
            "file_overrides": {
                definitions.to_string_lossy().to_string(): "\\newcommand{\\ZZ}{\\mathbb{Z}}\n"
            }
        });
        let mut output = Vec::new();
        run_ndjson(
            format!("{request}\n").as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let messages = responses(&output);
        let result = &messages[0]["result"];
        assert_eq!(messages[0]["status"], "ok", "{}", messages[0]);
        assert!(result["body_html"]
            .as_str()
            .unwrap()
            .contains(">Buffer</span>"));
        assert!(!result["body_html"]
            .as_str()
            .unwrap()
            .contains(">Disk</span>"));
        let zz = result["runtime"]["math"]["macros"]
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["name"] == "ZZ")
            .expect("unsaved include defines ZZ");
        assert_eq!(zz["body"], "\\mathbb{Z}");
        assert_eq!(
            result["converter"]["capabilities"]["multi_buffer_source"],
            true
        );
        assert!(result["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| dependency["kind"] == "include"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_request_errors_then_resynchronizes() {
        const TEST_LIMIT: usize = 256;
        let mut input = vec![b'x'; TEST_LIMIT + 1];
        input.extend_from_slice(b"\n");
        input.extend_from_slice(
            concat!(
                r#"{"protocol":"mathpreview.converter/v1","id":"next","path":"notes.md","source":"ok"}"#,
                "\n"
            )
            .as_bytes(),
        );
        let mut output = Vec::new();
        run_ndjson_with_limit(
            &input[..],
            &mut output,
            &ConvertCommandOptions::default(),
            TEST_LIMIT,
        )
        .unwrap();
        let messages = responses(&output);
        assert_eq!(messages[0]["error"]["code"], "request-too-large");
        assert_eq!(messages[1]["id"], "next");
        assert_eq!(messages[1]["status"], "ok");
    }

    #[test]
    fn unknown_and_explicit_converters_are_structured() {
        let input = concat!(
            r#"{"protocol":"mathpreview.converter/v1","id":"bad","path":"notes.md","source":"ok","converter":"pandoc"}"#,
            "\n",
            r##"{"protocol":"mathpreview.converter/v1","id":"forced","path":"notes.tex","source":"# Markdown","converter":"markdown"}"##,
            "\n",
        );
        let mut output = Vec::new();
        run_ndjson(
            input.as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();
        let messages = responses(&output);
        assert_eq!(messages[0]["error"]["code"], "unsupported-converter");
        assert_eq!(messages[1]["status"], "ok");
        assert_eq!(messages[1]["result"]["converter"]["format"], "markdown");
    }

    #[test]
    fn unsupported_converter_precedes_configuration_loading() {
        let dir = temp_dir("converter-error-precedence");
        let invalid_config = dir.join("invalid.toml");
        fs::write(&invalid_config, "[viewer\n").unwrap();
        let request = serde_json::json!({
            "protocol": CONVERSION_PROTOCOL_V1,
            "id": "unsupported-first",
            "path": dir.join("notes.md"),
            "source": "ok",
            "converter": "pandoc",
            "options": { "config_files": [invalid_config] }
        });
        let mut output = Vec::new();

        run_ndjson(
            format!("{request}\n").as_bytes(),
            &mut output,
            &ConvertCommandOptions::default(),
        )
        .unwrap();

        let message = &responses(&output)[0];
        assert_eq!(message["error"]["code"], "unsupported-converter");
        fs::remove_dir_all(dir).unwrap();
    }
}
