use clap::Parser;
use playwright::Playwright;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{transport::Server, Request, Response, Status};
use playwright::api::DocumentLoadState;
use chrono;

// Import the generated proto module
pub mod webcontent {
    include!(concat!(env!("OUT_DIR"), "/webcontent.rs"));
}
use webcontent::web_content_service_server::{WebContentService, WebContentServiceServer};
use webcontent::{ExtractContentRequest, ExtractContentResponse};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The port to run the gRPC server on
    #[arg(long, default_value = "50051")]
    port: u16,
    /// The path to the Chromium executable
    #[arg(long, value_name = "PATH")]
    browser_executable: Option<String>,
}

struct ExtractorService {
    browser: Arc<Mutex<playwright::api::Browser>>,
}

#[tonic::async_trait]
impl WebContentService for ExtractorService {
    async fn extract_content(
        &self,
        request: Request<ExtractContentRequest>,
    ) -> Result<Response<ExtractContentResponse>, Status> {
        let req = request.into_inner();
        let browser = self.browser.clone();
        let url = req.url.clone();
        let output_md = req.output_md;
        let use_openai = req.use_openai;
        let model = req.model;
        let prompt = req.prompt;
        let take_screenshot = req.take_screenshot;
        let mut html = String::new();
        let mut markdown = String::new();
        let mut screenshot = vec![];
        let mut openai_response = String::new();
        let mut error = String::new();
        let start = std::time::Instant::now();
        // Extraction logic
        let result = async {
            let browser = browser.lock().await;
            let context = browser.context_builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36")
                .build().await?;
            let page = context.new_page().await?;
            page.goto_builder(&url)
                .wait_until(DocumentLoadState::DomContentLoaded)
                .goto().await?;
            html = page.content().await?;
            // Remove <script>, <style>, <iframe>, <noscript> tags and their content using regex (no backreferences)
            let cleaned_html = {
                let mut cleaned = html.clone();
                for tag in ["script", "style", "iframe", "noscript"] {
                    let re = regex::Regex::new(&format!(r"(?is)<{0}[^>]*>.*?</{0}>", tag)).unwrap();
                    cleaned = re.replace_all(&cleaned, "").to_string();
                }
                cleaned
            };
            if take_screenshot {
                screenshot = page.screenshot_builder().full_page(true).screenshot().await?;
            }
            if use_openai {
                let openai_api_key = std::env::var("OPENAI_API_KEY").map_err(|_| Status::internal("OPENAI_API_KEY must be set"))?;
                let client = reqwest::Client::new();
                let prompt = if prompt.is_empty() {
                    "Summarize the following HTML content:".to_string()
                } else {
                    prompt
                };
                let user_content = format!("{}\n\nHTML:\n{}", prompt, cleaned_html);
                let body = serde_json::json!({
                    "model": model,
                    "messages": [
                        {"role": "user", "content": user_content}
                    ]
                });
                let resp = client.post("https://api.openai.com/v1/chat/completions")
                    .bearer_auth(openai_api_key)
                    .json(&body)
                    .send()
                    .await.map_err(|e| Status::internal(format!("OpenAI send error: {}", e)))?;
                let resp_json: serde_json::Value = resp.json().await.map_err(|e| Status::internal(format!("OpenAI json error: {}", e)))?;
                if let Some(answer) = resp_json["choices"][0]["message"]["content"].as_str() {
                    openai_response = answer.to_string();
                } else {
                    openai_response = format!("OpenAI API error or unexpected response: {:#?}", resp_json);
                }
            } else if output_md {
                let converter = htmd::HtmlToMarkdown::builder()
                    .skip_tags(vec!["script", "style", "iframe", "noscript"])
                    .build();
                match converter.convert(&html) {
                    Ok(md) => {
                        markdown = md.replace("\n", "<br>").trim().to_string();
                    },
                    Err(e) => {
                        error = format!("Failed to convert HTML to Markdown: {}", e);
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }.await;
        if let Err(e) = result {
            error = format!("{}", e);
        }
        let elapsed = start.elapsed();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        println!("[{}] [web-content-service] URL: {} | Elapsed time: {:.2?}", now, url, elapsed);
        Ok(Response::new(ExtractContentResponse {
            html,
            markdown,
            screenshot,
            openai_response,
            error,
        }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Use the provided browser executable or the default
    let browser_executable = cli.browser_executable.unwrap_or_else(|| "/Users/ivanarambula/Library/Caches/ms-playwright/chromium-1169/chrome-mac/Chromium.app/Contents/MacOS/Chromium".to_string());
    let playwright = Playwright::initialize().await?;
    let browser = playwright.chromium()
        .launcher()
        .executable(std::path::Path::new(&browser_executable))
        .headless(true)
        .launch()
        .await?;
    let service = ExtractorService {
        browser: Arc::new(Mutex::new(browser)),
    };
    // let addr = format!("[::1]:{}", cli.port).parse()?;
    let addr = format!("[0.0.0.0]:{}", cli.port).parse()?;
    println!("Starting gRPC server on {}", addr);
    Server::builder()
        .add_service(WebContentServiceServer::new(service))
        .serve(addr)
        .await?;
    Ok(())
}
