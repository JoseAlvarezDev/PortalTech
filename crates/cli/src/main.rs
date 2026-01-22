use anyhow::Result;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use shared::{TunnelMessage, TunnelResponse};
use std::time::Duration;
use tokio_tungstenite::tungstenite::protocol::Message;
use base64::Engine as _;
use ratatui::{
    backend::CrosstermBackend,
    widgets::{Block, Borders, Paragraph, List, ListItem},
    layout::{Layout, Constraint, Direction},
    style::{Style, Color, Modifier},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "8000")]
    port: u16,

    #[arg(short, long, default_value = "ws://127.0.0.1:3000/ws")]
    relay: String,

    #[arg(long, env = "PORTAL_AUTH_TOKEN", default_value = "portal-secret-123")]
    token: String,

    #[arg(long)]
    headless: bool,
}

struct App {
    port: u16,
    status: String,
    public_url: String,
    requests: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // UI Setup
    let mut terminal = if !args.headless {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        Some(Terminal::new(backend)?)
    } else {
        println!("PortalTech CLI (Headless Mode) - Local: {} | Relay: {}", args.port, args.relay);
        None
    };

    let mut app = App {
        port: args.port,
        status: "Connecting...".to_string(),
        public_url: "Pending...".to_string(),
        requests: Vec::new(),
    };

    let request = http::Request::builder()
        .uri(&args.relay)
        .header("X-Portal-Auth", args.token)
        .header("Host", "127.0.0.1:3000") // Required by some WS implementations
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .unwrap();

    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(10 * 1024 * 1024), // 10MB
        ..Default::default()
    };

    let (ws_stream, _) = tokio_tungstenite::connect_async_with_config(request, Some(config), false).await?;
    let (mut write, mut read) = ws_stream.split();
    app.status = "Connected".to_string();
    if args.headless { println!("Connected to relay server!"); }

    let client = reqwest::Client::new();

    loop {
        // Render UI
        if let Some(ref mut t) = terminal {
            t.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(5),
                        Constraint::Min(0),
                    ].as_ref())
                    .split(f.size());

                let header = Paragraph::new("PortalTech Rust Edition 🦀")
                    .style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL).title("🚀"));
                f.render_widget(header, chunks[0]);

                let info = Paragraph::new(vec![
                    ratatui::text::Line::from(vec![
                        ratatui::text::Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
                        ratatui::text::Span::raw(&app.status),
                    ]),
                    ratatui::text::Line::from(vec![
                        ratatui::text::Span::styled("Local:  ", Style::default().fg(Color::Cyan)),
                        ratatui::text::Span::raw(format!("http://localhost:{}", app.port)),
                    ]),
                    ratatui::text::Line::from(vec![
                        ratatui::text::Span::styled("Public: ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        ratatui::text::Span::raw(&app.public_url),
                    ]),
                ]).block(Block::default().borders(Borders::ALL).title(" Connection Info "));
                f.render_widget(info, chunks[1]);

                let items: Vec<ListItem> = app.requests.iter()
                    .map(|i| ListItem::new(i.as_str()))
                    .collect();
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Recent Traffic"))
                    .highlight_style(Style::default().add_modifier(Modifier::ITALIC));
                f.render_widget(list, chunks[2]);
            })?;
        }

        // Handle WS messages and input
        let quit = tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<TunnelMessage>(&text) {
                            Ok(TunnelMessage::Ready { url }) => {
                                app.public_url = url;
                                app.status = "Active".to_string();
                                if args.headless { println!("Tunnel Ready: {}", app.public_url); }
                                false
                            }
                            Ok(TunnelMessage::Request(req)) => {
                                let method = req.method.clone();
                                let path = req.path.clone();
                                app.requests.insert(0, format!("{} /{}", method, path));
                                
                                let local_url = format!("http://127.0.0.1:{}/{}", app.port, path);
                                if args.headless { println!("Forwarding to local: {}", local_url); }
                                
                                let mut rb = client.request(
                                    req.method.parse().unwrap_or(reqwest::Method::GET),
                                    &local_url
                                );

                                // Forward headers
                                for (k, v) in &req.headers {
                                    if k.to_lowercase() == "host" { continue; }
                                    rb = rb.header(k, v);
                                }

                                if let Some(ref _b) = req.body {
                                    let body_bytes = req.get_body_bytes();
                                    rb = rb.body(body_bytes);
                                }

                                let local_res = rb.send().await;

                                let tunnel_res = match local_res {
                                    Ok(res) => {
                                        let status = res.status().as_u16();
                                        if args.headless { println!("Local server responded: {}", status); }
                                        let mut headers = std::collections::HashMap::new();
                                        for (k, v) in res.headers() {
                                            headers.insert(k.to_string(), v.to_str().unwrap_or_default().to_string());
                                        }
                                        let body_bytes = res.bytes().await.unwrap_or_default();
                                        if body_bytes.len() > 5 * 1024 * 1024 {
                                            if args.headless { println!("Response too large ({} bytes). Rejecting.", body_bytes.len()); }
                                            TunnelResponse {
                                                request_id: req.id,
                                                status: 413,
                                                headers: std::collections::HashMap::new(),
                                                body: Some(format!("{{\"error\": \"Response body too large: {} bytes (limit 5MB)\"}}", body_bytes.len())),
                                            }
                                        } else {
                                            let body = if body_bytes.is_empty() { None } else { 
                                                Some(base64::engine::general_purpose::STANDARD.encode(&body_bytes)) 
                                            };
                                            TunnelResponse {
                                                request_id: req.id,
                                                status,
                                                headers,
                                                body,
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if args.headless { println!("Local forward error: {}", e); }
                                        TunnelResponse {
                                            request_id: req.id,
                                            status: 502,
                                            headers: std::collections::HashMap::new(),
                                            body: Some(format!("{{\"error\": \"Local server unreachable: {}\"}}", e)),
                                        }
                                    }
                                };

                                let resp_msg = TunnelMessage::Response(tunnel_res);
                                let _ = write.send(Message::Text(serde_json::to_string(&resp_msg).unwrap_or_default())).await;
                                false
                            }
                            _ => false,
                        }
                    }
                    _ => true, // Stream ended or error
                }
            }
            res = tokio::task::spawn_blocking(|| {
                if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    if let Event::Key(key) = event::read().unwrap() {
                        return Some(key.code);
                    }
                }
                None
            }), if !args.headless => {
                matches!(res, Ok(Some(KeyCode::Char('q'))))
            }
            else => {
                if args.headless {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    false
                } else {
                    true
                }
            }
        };

        if quit { break; }
    }

    // Cleanup
    if let Some(mut t) = terminal {
        disable_raw_mode()?;
        execute!(t.backend_mut(), LeaveAlternateScreen)?;
        t.show_cursor()?;
    }

    Ok(())
}
