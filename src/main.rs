use chrono;
use clap::Parser;
use playwright::api::Browser;
use playwright::api::BrowserContext;
use playwright::api::DocumentLoadState;
use playwright::Playwright;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tonic::{transport::Server, Request, Response, Status};

// Import the generated proto module
pub mod webcontent {
    include!(concat!(env!("OUT_DIR"), "/webcontent.rs"));
}
use webcontent::web_content_service_server::{WebContentService, WebContentServiceServer};
use webcontent::{ExtractContentRequest, ExtractContentResponse};

mod cache {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use chrono::Local;
    use directories::BaseDirs;
    use rusqlite::{params, Connection};
    use std::fs;
    use std::path::PathBuf;

    pub fn cache_dir_for_today() -> PathBuf {
        let base = BaseDirs::new().unwrap();
        let date = Local::now().format("%Y-%m-%d").to_string();
        base.cache_dir().join("web-content-extract").join(&date)
    }

    pub fn init_cache_db() -> Connection {
        let dir = cache_dir_for_today();
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("cache.db");
        let conn = Connection::open(db_path).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS cache (
                url TEXT PRIMARY KEY,
                date TEXT,
                file_path TEXT
            )",
            [],
        ).unwrap();
        conn
    }

    pub fn get_cached_html(url: &str) -> Option<(String, String)> {
        let conn = init_cache_db();
        let date = Local::now().format("%Y-%m-%d").to_string();
        let mut stmt = conn.prepare("SELECT file_path, date FROM cache WHERE url = ?1").ok()?;
        let mut rows = stmt.query(params![url]).ok()?;
        if let Some(row) = rows.next().ok()? {
            let file_path: String = row.get(0).ok()?;
            let cached_date: String = row.get(1).ok()?;
            if cached_date == date {
                let html = fs::read_to_string(&file_path).ok()?;
                return Some((html, file_path));
            }
        }
        None
    }

    pub fn set_cached_html(url: &str, html: &str) -> std::io::Result<()> {
        let dir = cache_dir_for_today();
        fs::create_dir_all(&dir)?;
        let file_name = URL_SAFE_NO_PAD.encode(url);
        let file_path = dir.join(format!("{}.html", file_name));
        fs::write(&file_path, html)?;
        let conn = init_cache_db();
        let date = Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT OR REPLACE INTO cache (url, date, file_path) VALUES (?1, ?2, ?3)",
            params![url, date, file_path.to_string_lossy()],
        ).unwrap();
        Ok(())
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The port to run the gRPC server on
    #[arg(long, default_value = "50051")]
    port: u16,
    /// The path to the Chromium executable
    #[arg(long, value_name = "PATH")]
    browser_executable: Option<String>,
    /// Use cache for website content
    #[arg(long, default_value_t = true)]
    use_cache: bool,
}

struct ContextPool {
    pool: Arc<Mutex<Vec<BrowserContext>>>,
}

impl ContextPool {
    async fn new(size: usize, browser: &playwright::api::Browser) -> Self {
        let mut contexts = Vec::with_capacity(size);
        for _ in 0..size {
            let context = browser.context_builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36")
                .build().await.unwrap();
            contexts.push(context);
        }
        Self {
            pool: Arc::new(Mutex::new(contexts)),
        }
    }

    async fn acquire(&self) -> BrowserContext {
        loop {
            let mut pool = self.pool.lock().await;
            if let Some(context) = pool.pop() {
                return context;
            }
            drop(pool);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn release(&self, context: BrowserContext) {
        let mut pool = self.pool.lock().await;
        pool.push(context);
    }
}

impl Clone for ContextPool {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

struct SharedState {
    browser: Option<Browser>,
    context_pool: Option<ContextPool>,
}

type SharedStateArc = Arc<Mutex<SharedState>>;

struct ExtractorService {
    shared_state: SharedStateArc,
    browser_executable: String,
    pool_size: usize,
    playwright: Playwright,
    use_cache: bool,
}

impl ExtractorService {
    // No-op: browser reset is now only on error
    async fn maybe_reset_browser(&self) {}
    async fn get_context_pool(&self) -> ContextPool {
        let state = self.shared_state.lock().await;
        state.context_pool.as_ref().unwrap().clone()
    }

    async fn fetch_direct(&self, url: &str) -> Result<String, String> {
        let client = reqwest::Client::new();
        let resp = client.get(url).send().await.map_err(|e| format!("Direct fetch error: {}", e))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| format!("Direct fetch body error: {}", e))?;
        // Heuristic for blocked/invalid content
        if !status.is_success() {
            return Err(format!("Direct fetch HTTP error: {}", status));
        }
        let lower = body.to_lowercase();
        if lower.contains("access denied") || lower.contains("captcha") || lower.contains("blocked") || lower.contains("cloudflare") || lower.trim().is_empty() {
            return Err("Blocked or invalid content detected".to_string());
        }
        Ok(body)
    }

    async fn fetch_playwright(&self, context_pool: ContextPool, url: &str, take_screenshot: bool) -> Result<(String, Vec<u8>), String> {
        let context = context_pool.acquire().await;
        let page = context.new_page().await.map_err(|e| format!("Playwright new_page error: {}", e))?;
        page.goto_builder(url)
            .wait_until(DocumentLoadState::DomContentLoaded)
            .goto().await.map_err(|e| format!("Playwright goto error: {}", e))?;
        // Retry page.content() up to 3 times if 'Execution context was destroyed' error occurs
        let mut html = None;
        for _ in 0..3 {
            match page.content().await {
                Ok(h) => {
                    html = Some(h);
                    break;
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    if msg.contains("Execution context was destroyed") {
                        // Wait a bit and retry
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    } else {
                        context_pool.release(context).await;
                        return Err(format!("Content error: {}", msg));
                    }
                }
            }
        }
        let html = html.ok_or_else(|| "Content error: Execution context was destroyed after retries".to_string())?;
        let screenshot = if take_screenshot {
            match page.screenshot_builder().full_page(true).screenshot().await {
                Ok(shot) => {
                    log_to_file_and_stdout(&format!(
                        "[{}] [web-content-service] Screenshot SUCCESS ({} bytes)\n",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        shot.len()
                    ));
                    shot
                },
                Err(e) => {
                    log_to_file_and_stdout(&format!(
                        "[{}] [web-content-service] Screenshot FAILED: {}\n",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        e
                    ));
                    context_pool.release(context).await;
                    return Err(format!("Screenshot error: {}", e));
                }
            }
        } else {
            vec![]
        };
        context_pool.release(context).await;
        Ok((html, screenshot))
    }

    async fn reset_browser(&self) {
        let mut state = self.shared_state.lock().await;
        // Drop old browser and context pool
        state.browser = None;
        state.context_pool = None;
        // Create new browser and context pool
        let browser = self.playwright.chromium()
            .launcher()
            .executable(std::path::Path::new(&self.browser_executable))
            .headless(true)
            .launch()
            .await
            .expect("Failed to relaunch browser");
        let context_pool = ContextPool::new(self.pool_size, &browser).await;
        state.browser = Some(browser);
        state.context_pool = Some(context_pool);
        println!("[web-content-service] Browser and context pool reset due to timeout");
    }

    async fn fetch_playwright_with_retries(&self, url: &str, take_screenshot: bool) -> Result<(String, Vec<u8>), String> {
        let mut last_err = None;
        for attempt in 0..3 {
            let context_pool = self.get_context_pool().await;
            let result = self.fetch_playwright(context_pool, url, take_screenshot).await;
            match &result {
                Ok(_) => return result,
                Err(e) => {
                    let err_lc = e.to_lowercase();
                    if err_lc.contains("timeout") || err_lc.contains("execution context was destroyed") {
                        println!("[web-content-service] Error (timeout or context destroyed) on attempt {} for {}. Resetting browser and retrying...", attempt + 1, url);
                        self.reset_browser().await;
                        last_err = Some(e.clone());
                        continue;
                    } else {
                        return Err(e.clone());
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| "Failed after 3 attempts".to_string()))
    }
}

#[tonic::async_trait]
impl WebContentService for ExtractorService {
    type StreamLogsStream = Pin<Box<dyn tokio_stream::Stream<Item = Result<webcontent::LogLine, tonic::Status>> + Send + 'static>>;

    async fn stream_logs(
        &self,
        _request: Request<webcontent::LogRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        // Path to the log file
        let log_path = "web-content-service.log";
        // Open the log file and seek to the end
        let file = File::open(log_path).map_err(|e| Status::internal(format!("Failed to open log file: {}", e)))?;
        let mut reader = BufReader::new(file);
        let _ = reader.seek(SeekFrom::End(0));
        // Use a channel to send new lines
        let (tx, rx) = mpsc::channel(100);
        // Spawn a task to tail the log file
        tokio::spawn(async move {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        // No new line, wait a bit
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                    Ok(_) => {
                        let _ = tx.send(Ok(webcontent::LogLine { line: line.clone() })).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(format!("Log read error: {}", e)))).await;
                        break;
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)) as Self::StreamLogsStream))
    }

    async fn extract_content(
        &self,
        request: Request<ExtractContentRequest>,
    ) -> Result<Response<ExtractContentResponse>, Status> {
        self.maybe_reset_browser().await;
        let req = request.into_inner();
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

        // Only use cache for non-screenshot requests
        if self.use_cache && !take_screenshot {
            if let Some((cached_html, _file_path)) = cache::get_cached_html(&url) {
                html = cached_html;
                let elapsed = start.elapsed();
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                log_to_file_and_stdout(&format!("[{}] [web-content-service] URL: {} | Elapsed time: {:.2?} | Used: cache\n", now, url, elapsed));
                return Ok(Response::new(ExtractContentResponse {
                    html,
                    markdown,
                    screenshot,
                    openai_response,
                    error,
                }));
            }
        }

        let mut used_playwright = false;
        let html_ok;
        if take_screenshot {
            // Always use Playwright if screenshot is requested
            match self.fetch_playwright_with_retries(&url, take_screenshot).await {
                Ok((body, shot)) => {
                    html = body;
                    screenshot = shot;
                    used_playwright = true;
                    html_ok = true;
                }
                Err(e) => {
                    error = e;
                    screenshot = vec![];
                    html_ok = false;
                }
            }
        } else {
            // Parallel extraction logic
            let direct_fut = self.fetch_direct(&url);
            let playwright_fut = self.fetch_playwright_with_retries(&url, take_screenshot);
            tokio::pin!(direct_fut);
            tokio::pin!(playwright_fut);
            tokio::select! {
                direct = &mut direct_fut => {
                    match direct {
                        Ok(body) => {
                            html = body;
                            screenshot = vec![];
                            html_ok = true;
                        },
                        Err(_) => {
                            // Wait for playwright
                            match playwright_fut.await {
                                Ok((body, shot)) => {
                                    html = body;
                                    screenshot = shot;
                                    used_playwright = true;
                                    html_ok = true;
                                },
                                Err(e) => {
                                    error = e;
                                    screenshot = vec![];
                                    html_ok = false;
                                }
                            }
                        }
                    }
                },
                playwright = &mut playwright_fut => {
                    match playwright {
                        Ok((body, shot)) => {
                            html = body;
                            screenshot = shot;
                            used_playwright = true;
                            html_ok = true;
                        },
                        Err(e) => {
                            error = e;
                            screenshot = vec![];
                            html_ok = false;
                        }
                    }
                }
            }
        }
        // Store in cache after successful fetch if enabled (server-side) and not a screenshot request
        if html_ok && self.use_cache && !take_screenshot {
            let _ = cache::set_cached_html(&url, &html);
        }
        if html_ok {
            // Remove <script>, <style>, <iframe>, <noscript> tags and their content using regex (no backreferences)
            let cleaned_html = {
                let mut cleaned = html.clone();
                for tag in ["script", "style", "iframe", "noscript"] {
                    let re = regex::Regex::new(&format!(r"(?is)<{0}[^>]*>.*?</{0}>", tag)).unwrap();
                    cleaned = re.replace_all(&cleaned, "").to_string();
                }
                cleaned
            };
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
        }
        if take_screenshot && screenshot.is_empty() && error.is_empty() {
            error = "Failed to capture screenshot".to_string();
        }
        let elapsed = start.elapsed();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        log_to_file_and_stdout(&format!("[{}] [web-content-service] URL: {} | Elapsed time: {:.2?} | Used: {}\n", now, url, elapsed, if used_playwright {"playwright"} else {"direct"}));
        Ok(Response::new(ExtractContentResponse {
            html,
            markdown,
            screenshot,
            openai_response,
            error,
        }))
    }
}

fn log_to_file_and_stdout(msg: &str) {
    print!("{}", msg);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("web-content-service.log")
        .unwrap();
    let _ = file.write_all(msg.as_bytes());
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let browser_executable = cli.browser_executable.unwrap_or_else(|| "/Users/ivanarambula/Library/Caches/ms-playwright/chromium-1169/chrome-mac/Chromium.app/Contents/MacOS/Chromium".to_string());
    let playwright = Playwright::initialize().await?;
    let browser = playwright.chromium()
        .launcher()
        .executable(std::path::Path::new(&browser_executable))
        .headless(true)
        .launch()
        .await?;
    let pool_size = 4;
    let context_pool = ContextPool::new(pool_size, &browser).await;
    let shared_state = Arc::new(Mutex::new(SharedState {
        browser: Some(browser),
        context_pool: Some(context_pool),
    }));
    let service = ExtractorService {
        shared_state: shared_state.clone(),
        browser_executable,
        pool_size,
        playwright,
        use_cache: cli.use_cache,
    };
    let addr = format!("[::0]:{}", cli.port).parse()?;
    log_to_file_and_stdout(&format!("Starting gRPC server on {}\n", addr));
    Server::builder()
        .add_service(WebContentServiceServer::new(service))
        .serve(addr)
        .await?;
    Ok(())
}
