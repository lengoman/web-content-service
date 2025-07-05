use clap::Parser;
use std::fs::File;
use std::io::Write;

pub mod webcontent {
    include!(concat!(env!("OUT_DIR"), "/webcontent.rs"));
}
use webcontent::web_content_service_client::WebContentServiceClient;
use webcontent::ExtractContentRequest;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The URL to extract content from (optional if --show-logs)
    #[arg(long)]
    url: Option<String>,
    /// Output markdown
    #[arg(long, default_value_t = false)]
    output_md: bool,
    /// Use OpenAI
    #[arg(long, default_value_t = false)]
    use_openai: bool,
    /// OpenAI model
    #[arg(long, default_value = "gpt-3.5-turbo")]
    model: String,
    /// Prompt for OpenAI
    #[arg(long, default_value = "")]
    prompt: String,
    /// Take screenshot
    #[arg(long, default_value_t = false)]
    take_screenshot: bool,
    /// gRPC server host
    #[arg(long, default_value = "[::1]")]
    host: String,
    /// gRPC server port
    #[arg(long, default_value_t = 50051)]
    port: u16,
    /// Show logs from the server
    #[arg(long, default_value_t = false)]
    show_logs: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let addr = format!("http://{}:{}", cli.host, cli.port);
    let mut client = WebContentServiceClient::connect(addr).await?;
    if cli.show_logs {
        let mut stream = client.stream_logs(tonic::Request::new(webcontent::LogRequest {})).await?.into_inner();
        println!("--- Streaming logs from server ---");
        while let Some(line) = stream.message().await? {
            print!("{}", line.line);
        }
        return Ok(());
    }
    // Require --url if not showing logs
    let url = match cli.url {
        Some(u) => u,
        None => {
            eprintln!("Error: --url is required unless --show-logs is set");
            std::process::exit(1);
        }
    };
    let request = tonic::Request::new(ExtractContentRequest {
        url,
        output_md: cli.output_md,
        use_openai: cli.use_openai,
        model: cli.model,
        prompt: cli.prompt,
        take_screenshot: cli.take_screenshot,
        use_cache: false, // always false, server controls cache
    });
    let response = client.extract_content(request).await?.into_inner();
    if !response.error.is_empty() {
        eprintln!("Error: {}", response.error);
        std::process::exit(1);
    }
    if cli.take_screenshot && !response.screenshot.is_empty() {
        let mut file = File::create("image.png")?;
        file.write_all(&response.screenshot)?;
        println!("Screenshot saved to image.png");
        return Ok(());
    }
    if cli.use_openai && !response.openai_response.is_empty() {
        println!("{}", response.openai_response);
        return Ok(());
    }
    if cli.output_md && !response.markdown.is_empty() {
        println!("{}", response.markdown);
        return Ok(());
    }
    // Default: print HTML
    println!("{}", response.html);
    Ok(())
} 