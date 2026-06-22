use std::env;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use serde_json::{Value, json};

const TEXT_LIMIT: usize = 4096;
const CAPTION_LIMIT: usize = 1024;

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "webm", "mkv", "avi"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CliError {}

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Args {
    text: String,
    file: Option<String>,
    help: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    token: String,
    chat_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaType {
    method: &'static str,
    field: &'static str,
}

fn usage(program_name: &str) -> String {
    [
        "Usage:",
        &format!("  {program_name} --text \"hello\""),
        &format!("  {program_name} --file ./image.png"),
        &format!("  {program_name} --text \"caption\" --file ./video.mp4"),
        "",
        "Environment:",
        "  TG_BOT_TOKEN   Telegram bot token (required)",
        "  TG_CHAT_ID     Target chat ID or @channelusername (required)",
        "",
        "Notes:",
        "  - At least one of --text or --file is required.",
        "  - --file is auto-detected as image or video from its extension.",
        "  - If --text is too long for a media caption, the file is sent first and the text is sent as follow-up messages.",
    ]
    .join("\n")
}

fn resolve_program_name(raw_program_name: &str) -> String {
    let program_name = Path::new(raw_program_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");

    if program_name == "tg_push.py" {
        "python3 -m tg_push".to_string()
    } else if program_name.is_empty() {
        "tg-push".to_string()
    } else {
        program_name.to_string()
    }
}

fn parse_args(argv: &[String], program_name: &str) -> Result<Args> {
    let mut args = Args::default();
    let mut index = 0;

    while index < argv.len() {
        let current = argv[index].as_str();

        if current == "--help" || current == "-h" {
            args.help = true;
            index += 1;
            continue;
        }

        if current != "--text" && current != "--file" {
            return Err(CliError::new(format!(
                "Unknown argument: {current}\n\n{}",
                usage(program_name)
            )));
        }

        if index + 1 >= argv.len() {
            return Err(CliError::new(format!(
                "Missing value for {current}\n\n{}",
                usage(program_name)
            )));
        }

        let value = argv[index + 1].clone();
        if current == "--text" {
            args.text = value;
        } else {
            args.file = Some(value);
        }

        index += 2;
    }

    Ok(args)
}

fn get_config() -> Result<Config> {
    let token = env::var("TG_BOT_TOKEN").unwrap_or_default();
    let chat_id = env::var("TG_CHAT_ID").unwrap_or_default();

    if token.is_empty() {
        return Err(CliError::new(
            "TG_BOT_TOKEN is not set.\n\
             Get a token from @BotFather on Telegram: https://t.me/BotFather\n\
             Then set it: export TG_BOT_TOKEN=<your-token>",
        ));
    }

    if chat_id.is_empty() {
        return Err(CliError::new(
            "TG_CHAT_ID is not set.\n\
             For a 1:1 bot chat, use your Telegram user ID (send /start to @userinfobot to find it).\n\
             For a group or channel, use its numeric ID or @username.\n\
             Then set it: export TG_CHAT_ID=<your-chat-id>",
        ));
    }

    Ok(Config { token, chat_id })
}

fn detect_media_type(file_path: &Path) -> Result<MediaType> {
    let extension = file_path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return Ok(MediaType {
            method: "sendPhoto",
            field: "photo",
        });
    }

    if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Ok(MediaType {
            method: "sendVideo",
            field: "video",
        });
    }

    let shown_extension = if extension.is_empty() {
        "(no extension)".to_string()
    } else {
        format!(".{extension}")
    };

    Err(CliError::new(format!(
        "Unsupported file type: {shown_extension}\n\
         Supported image extensions: .jpg, .jpeg, .png, .webp\n\
         Supported video extensions: .mp4, .mov, .m4v, .webm, .mkv, .avi"
    )))
}

fn telegram_json_request(
    client: &Client,
    token: &str,
    method: &str,
    json_payload: Value,
) -> Result<Value> {
    let response = client
        .post(telegram_url(token, method))
        .json(&json_payload)
        .send()
        .map_err(|error| transport_error(error, token))?;

    parse_telegram_response(response, token)
}

fn telegram_multipart_request(
    client: &Client,
    token: &str,
    method: &str,
    form_payload: Form,
) -> Result<Value> {
    let response = client
        .post(telegram_url(token, method))
        .multipart(form_payload)
        .send()
        .map_err(|error| transport_error(error, token))?;

    parse_telegram_response(response, token)
}

fn telegram_url(token: &str, method: &str) -> String {
    format!("https://api.telegram.org/bot{token}/{method}")
}

fn transport_error(error: reqwest::Error, token: &str) -> CliError {
    CliError::new(format!(
        "Telegram API request failed: {}",
        redact_token(&error.to_string(), token)
    ))
}

fn parse_telegram_response(response: reqwest::blocking::Response, token: &str) -> Result<Value> {
    let status = response.status().as_u16();
    let response_text = response
        .text()
        .map_err(|error| transport_error(error, token))?;

    let payload: Value = serde_json::from_str(&response_text).map_err(|_| {
        CliError::new(format!(
            "Telegram API returned non-JSON response ({status}): {response_text}"
        ))
    })?;

    let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if status >= 400 || !ok {
        let mut description = payload
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Telegram API request failed with status {status}"));

        if status == 401 {
            description.push_str("\nCheck that TG_BOT_TOKEN is correct (get one from @BotFather).");
        } else if status == 400 && description.to_ascii_lowercase().contains("chat not found") {
            description.push_str("\nCheck that TG_CHAT_ID is correct.");
        }

        return Err(CliError::new(description));
    }

    Ok(payload.get("result").cloned().unwrap_or(Value::Null))
}

fn send_text(client: &Client, token: &str, chat_id: &str, text: &str) -> Result<()> {
    for chunk in split_text(text, TEXT_LIMIT) {
        telegram_json_request(
            client,
            token,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": chunk,
            }),
        )?;
    }

    Ok(())
}

fn send_file(
    client: &Client,
    token: &str,
    chat_id: &str,
    file_name: &str,
    text: &str,
) -> Result<()> {
    let file_path = expand_user_path(file_name);

    if !file_path.exists() {
        return Err(CliError::new(format!(
            "File not found: {}",
            file_path.display()
        )));
    }

    if !file_path.is_file() {
        return Err(CliError::new(format!(
            "Not a file: {}",
            file_path.display()
        )));
    }

    let media_type = detect_media_type(&file_path)?;
    let caption = caption_for_text(text);
    let file_bytes = std::fs::read(&file_path).map_err(|error| {
        CliError::new(format!(
            "Failed to read file {}: {error}",
            file_path.display()
        ))
    })?;

    let file_name = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload")
        .to_string();

    let part = Part::bytes(file_bytes)
        .file_name(file_name)
        .mime_str(mime_type_for_path(&file_path))
        .map_err(|error| CliError::new(format!("Failed to build multipart upload: {error}")))?;

    let mut form = Form::new().text("chat_id", chat_id.to_string());
    if let Some(caption) = caption {
        form = form.text("caption", caption.to_string());
    }
    form = form.part(media_type.field, part);

    telegram_multipart_request(client, token, media_type.method, form)?;

    if !text.is_empty() && caption.is_none() {
        send_text(client, token, chat_id, text)?;
    }

    Ok(())
}

fn split_text(text: &str, limit: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for character in text.chars() {
        if current_len == limit {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }

        current.push(character);
        current_len += 1;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn caption_for_text(text: &str) -> Option<&str> {
    if !text.is_empty() && text.chars().count() <= CAPTION_LIMIT {
        Some(text)
    } else {
        None
    }
}

fn expand_user_path(path: &str) -> PathBuf {
    if path == "~"
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home);
    }

    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    PathBuf::from(path)
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

fn redact_token(message: &str, token: &str) -> String {
    if token.is_empty() {
        message.to_string()
    } else {
        message.replace(token, "<redacted>")
    }
}

pub fn run_from_env() -> i32 {
    let mut raw_args = env::args();
    let program_name = resolve_program_name(&raw_args.next().unwrap_or_default());
    let argv: Vec<String> = raw_args.collect();

    match run(argv, &program_name) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run(argv: Vec<String>, program_name: &str) -> Result<()> {
    let args = parse_args(&argv, program_name)?;

    if args.help {
        println!("{}", usage(program_name));
        return Ok(());
    }

    if args.text.is_empty() && args.file.is_none() {
        return Err(CliError::new(format!(
            "At least one of --text or --file is required.\n\n{}",
            usage(program_name)
        )));
    }

    let config = get_config()?;
    let client = Client::new();

    if let Some(file) = args.file {
        send_file(&client, &config.token, &config.chat_id, &file, &args.text)
    } else {
        send_text(&client, &config.token, &config.chat_id, &args.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parses_text_and_file() {
        let args = parse_args(
            &argv(&["--text", "latest render", "--file", "./render.png"]),
            "tg-push",
        )
        .unwrap();

        assert_eq!(args.text, "latest render");
        assert_eq!(args.file.as_deref(), Some("./render.png"));
        assert!(!args.help);
    }

    #[test]
    fn parses_help_without_requiring_other_args() {
        let args = parse_args(&argv(&["--help"]), "tg-push").unwrap();

        assert!(args.help);
    }

    #[test]
    fn rejects_unknown_argument() {
        let error = parse_args(&argv(&["--message", "hello"]), "tg-push").unwrap_err();

        assert!(error.to_string().contains("Unknown argument: --message"));
    }

    #[test]
    fn rejects_missing_value() {
        let error = parse_args(&argv(&["--file"]), "tg-push").unwrap_err();

        assert!(error.to_string().contains("Missing value for --file"));
    }

    #[test]
    fn detects_images_and_videos_case_insensitively() {
        let image = detect_media_type(Path::new("render.PNG")).unwrap();
        let video = detect_media_type(Path::new("clip.MOV")).unwrap();

        assert_eq!(image.method, "sendPhoto");
        assert_eq!(image.field, "photo");
        assert_eq!(video.method, "sendVideo");
        assert_eq!(video.field, "video");
    }

    #[test]
    fn rejects_unsupported_file_types() {
        let error = detect_media_type(Path::new("archive.zip")).unwrap_err();

        assert!(error.to_string().contains("Unsupported file type: .zip"));
    }

    #[test]
    fn splits_text_on_character_count() {
        let chunks = split_text("abcd", 2);

        assert_eq!(chunks, vec!["ab", "cd"]);
    }

    #[test]
    fn splits_unicode_without_byte_slicing() {
        let chunks = split_text("a😀b😀c", 2);

        assert_eq!(chunks, vec!["a😀", "b😀", "c"]);
    }

    #[test]
    fn captions_are_used_only_within_telegram_limit() {
        assert_eq!(caption_for_text("hello"), Some("hello"));
        assert_eq!(caption_for_text(""), None);
        assert_eq!(caption_for_text(&"x".repeat(CAPTION_LIMIT + 1)), None);
    }

    #[test]
    fn redacts_token_from_transport_errors() {
        let message = redact_token(
            "request failed for https://api.telegram.org/bot123:abc/sendMessage",
            "123:abc",
        );

        assert_eq!(
            message,
            "request failed for https://api.telegram.org/bot<redacted>/sendMessage"
        );
    }
}
