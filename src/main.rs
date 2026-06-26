/// Tunnel Client v2
///
/// Connects to a tunnel server using protocol v2 (multiplexed binary framing).
/// Each incoming REQUEST frame is handled concurrently in a separate tokio task.
/// Supports HTTP/1.1 and HTTP/2 browser requests transparently.

use bytes::{Bytes, BytesMut, BufMut};
use clap::{Arg, Command};
use http_body_util::BodyExt;
use rustls::{ClientConfig, RootCertStore, crypto::CryptoProvider, pki_types::ServerName};
use rustls::pki_types::pem::PemObject;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tracing::{debug, info, warn};

const DEFAULT_RECONNECT_INTERVAL_SECS: u64 = 1;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

// ─── Protocol v2 (inlined from rpxy-lib/src/tunnel/protocol.rs) ──────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FrameType {
  Connect  = 0x01,
  Config   = 0x02,
  Request  = 0x03,
  Response = 0x04,
  Data     = 0x05,
  Reset    = 0x06,
  Ping     = 0x07,
  Pong     = 0x08,
  GoAway   = 0x09,
}

impl FrameType {
  fn from_u8(b: u8) -> Option<Self> {
    match b {
      0x01 => Some(FrameType::Connect),
      0x02 => Some(FrameType::Config),
      0x03 => Some(FrameType::Request),
      0x04 => Some(FrameType::Response),
      0x05 => Some(FrameType::Data),
      0x06 => Some(FrameType::Reset),
      0x07 => Some(FrameType::Ping),
      0x08 => Some(FrameType::Pong),
      0x09 => Some(FrameType::GoAway),
      _    => None,
    }
  }
}

mod flags {
  pub const HAS_BODY: u8        = 0x01;
  pub const END_STREAM: u8      = 0x01;
  pub const RESPONSE_HAS_BODY: u8 = 0x01;
}

mod caps {
  pub const HTTP2_BACKENDS: u8 = 0x01;
}

mod reset_codes {
  pub const BACKEND_UNREACHABLE: u16 = 0x01;
  pub const INTERNAL_ERROR: u16     = 0x03;
}

#[derive(Debug, Clone)]
struct Frame {
  frame_type: FrameType,
  stream_id:  u32,
  flags:      u8,
  payload:    Bytes,
}

impl Frame {
  fn has_flag(&self, f: u8) -> bool { self.flags & f != 0 }
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Frame> {
  let mut hdr = [0u8; 10];
  r.read_exact(&mut hdr).await?;
  let ft = FrameType::from_u8(hdr[0])
    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unknown frame type 0x{:02x}", hdr[0])))?;
  let stream_id = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
  let flags     = hdr[5];
  let length    = u32::from_be_bytes([hdr[6], hdr[7], hdr[8], hdr[9]]) as usize;
  if length > MAX_PAYLOAD {
    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame payload too large"));
  }
  let mut payload = vec![0u8; length];
  r.read_exact(&mut payload).await?;
  Ok(Frame { frame_type: ft, stream_id, flags, payload: Bytes::from(payload) })
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> std::io::Result<()> {
  let len = frame.payload.len() as u32;
  let mut hdr = [0u8; 10];
  hdr[0] = frame.frame_type as u8;
  hdr[1..5].copy_from_slice(&frame.stream_id.to_be_bytes());
  hdr[5] = frame.flags;
  hdr[6..10].copy_from_slice(&len.to_be_bytes());
  w.write_all(&hdr).await?;
  w.write_all(&frame.payload).await?;
  Ok(())
}

// ─── Payload builders ─────────────────────────────────────────────────────────

fn build_connect_payload(tunnel_id: &str, api_key: &str) -> Bytes {
  let mut buf = BytesMut::new();
  buf.put_u8(0x02); // protocol version 2
  buf.put_u8(caps::HTTP2_BACKENDS);
  buf.put_u16(tunnel_id.len() as u16);
  buf.extend_from_slice(tunnel_id.as_bytes());
  buf.put_u16(api_key.len() as u16);
  buf.extend_from_slice(api_key.as_bytes());
  buf.freeze()
}

fn build_response_payload(status_code: u16, headers: &[(String, String)]) -> Bytes {
  let mut buf = BytesMut::new();
  buf.put_u16(status_code);
  buf.put_u16(headers.len() as u16);
  for (name, value) in headers {
    let nb = name.as_bytes();
    buf.put_u8(nb.len().min(255) as u8);
    buf.extend_from_slice(&nb[..nb.len().min(255)]);
    let vb = value.as_bytes();
    buf.put_u16(vb.len().min(65535) as u16);
    buf.extend_from_slice(&vb[..vb.len().min(65535)]);
  }
  buf.freeze()
}

// ─── Payload parsers ──────────────────────────────────────────────────────────

struct ConfigPayload {
  domains: Vec<DomainEntry>,
}
struct DomainEntry {
  domain_id: u16,
  _flags: u8,
  _domain: String,
  local_host: String,
}

struct RequestPayload {
  domain_id: u16,
  method: String,
  path: String,
  headers: Vec<(String, String)>,
}

fn parse_config(payload: &[u8]) -> Option<ConfigPayload> {
  if payload.len() < 2 { return None; }
  let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
  let mut pos = 2;
  let mut domains = Vec::with_capacity(count);
  for _ in 0..count {
    if pos + 3 > payload.len() { return None; }
    let domain_id = u16::from_be_bytes([payload[pos], payload[pos+1]]); pos += 2;
    let flags = payload[pos]; pos += 1;
    let domain = read_u16_str(payload, &mut pos)?;
    let local_host = read_u16_str(payload, &mut pos)?;
    domains.push(DomainEntry { domain_id, _flags: flags, _domain: domain, local_host });
  }
  Some(ConfigPayload { domains })
}

fn parse_request(payload: &[u8]) -> Option<RequestPayload> {
  if payload.len() < 2 { return None; }
  let domain_id = u16::from_be_bytes([payload[0], payload[1]]);
  let mut pos = 2;
  let method  = read_u8_str(payload, &mut pos)?;
  let path    = read_u16_str(payload, &mut pos)?;
  let headers = read_headers(payload, &mut pos)?;
  Some(RequestPayload { domain_id, method, path, headers })
}

fn read_u8_str(p: &[u8], pos: &mut usize) -> Option<String> {
  if *pos >= p.len() { return None; }
  let len = p[*pos] as usize; *pos += 1;
  if *pos + len > p.len() { return None; }
  let s = std::str::from_utf8(&p[*pos..*pos+len]).ok()?.to_string();
  *pos += len;
  Some(s)
}

fn read_u16_str(p: &[u8], pos: &mut usize) -> Option<String> {
  if *pos + 2 > p.len() { return None; }
  let len = u16::from_be_bytes([p[*pos], p[*pos+1]]) as usize; *pos += 2;
  if *pos + len > p.len() { return None; }
  let s = std::str::from_utf8(&p[*pos..*pos+len]).ok()?.to_string();
  *pos += len;
  Some(s)
}

fn read_headers(p: &[u8], pos: &mut usize) -> Option<Vec<(String, String)>> {
  if *pos + 2 > p.len() { return None; }
  let count = u16::from_be_bytes([p[*pos], p[*pos+1]]) as usize; *pos += 2;
  let mut headers = Vec::with_capacity(count);
  for _ in 0..count {
    let name  = read_u8_str(p, pos)?;
    let value = read_u16_str(p, pos)?;
    headers.push((name, value));
  }
  Some(headers)
}

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Args {
  api_url: String,
  tunnel_id: String,
  api_key: String,
  reconnect_interval: u64,
  no_tls: bool,
  tls_server_name: Option<String>,
  tls_ca_cert_path: Option<String>,
  verbose: bool,
}

impl Args {
  fn parse() -> Result<Self, Box<dyn std::error::Error>> {
    let matches = Command::new("tunnel-client")
      .version("2.0")
      .about("Tunnel client (protocol v2) for rust-rpxy")
      .arg(Arg::new("api-url").short('a').long("api-url").required(true).help("Proxy-admin API base URL"))
      .arg(Arg::new("tunnel-id").short('t').long("tunnel-id").required(true).help("Tunnel ID"))
      .arg(Arg::new("api-key").short('k').long("api-key").required(true).help("API key (<subscriptionId>_<salt>)"))
      .arg(Arg::new("reconnect-interval").short('r').long("reconnect-interval").default_value("1").help("Reconnect interval (seconds)"))
      .arg(Arg::new("no-tls").long("no-tls").action(clap::ArgAction::SetTrue)
        .help("Use plain TCP (no TLS) — for ESP32 or local testing. Server must listen on tunnel_port (not tunnel_port_tls)"))
      .arg(Arg::new("tls-server-name").long("tls-server-name").required(false).help("Override TLS SNI hostname"))
      .arg(Arg::new("tls-ca-cert-path").long("tls-ca-cert-path").required(false).help("Custom CA cert for tunnel TLS"))
      .arg(Arg::new("verbose").short('v').long("verbose").action(clap::ArgAction::SetTrue).help("Debug logging"))
      .get_matches();

    let reconnect_interval = matches.get_one::<String>("reconnect-interval").unwrap()
      .parse().unwrap_or(DEFAULT_RECONNECT_INTERVAL_SECS);

    Ok(Args {
      api_url: matches.get_one::<String>("api-url").unwrap().clone(),
      tunnel_id: matches.get_one::<String>("tunnel-id").unwrap().clone(),
      api_key: matches.get_one::<String>("api-key").unwrap().clone(),
      reconnect_interval,
      no_tls: matches.get_flag("no-tls"),
      tls_server_name: matches.get_one::<String>("tls-server-name").cloned(),
      tls_ca_cert_path: matches.get_one::<String>("tls-ca-cert-path").cloned(),
      verbose: matches.get_flag("verbose"),
    })
  }
}

// ─── API: validate tunnel and get proxy address ───────────────────────────────

async fn fetch_tunnel_info(api_url: &str, tunnel_id: &str, api_key: &str) -> Result<String, String> {
  use http_body_util::Full;
  let url = format!("{}/client-tunnel/{}", api_url, tunnel_id);
  let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
  let client: hyper_util::client::legacy::Client<_, Full<Bytes>> =
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
  let req = hyper::Request::builder().method("GET").uri(&url)
    .header("Authorization", format!("Bearer {}", api_key)).body(Full::new(Bytes::new()))
    .map_err(|e| format!("Request build error: {}", e))?;
  let res = client.request(req).await.map_err(|e| format!("HTTP error: {}", e))?;
  let status = res.status();
  let body = String::from_utf8_lossy(&res.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes()).to_string();
  if status.is_success() {
    extract_string_field(&body, "proxyAddress").ok_or_else(|| "No proxyAddress in response".to_string())
  } else {
    Err(format!("API error {}: {}", status, body))
  }
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
  let key = format!("\"{}\"", field);
  let start = json.find(&key)?;
  let after = json[start + key.len()..].trim_start();
  let after = after.strip_prefix(':')?.trim_start();
  if let Some(stripped) = after.strip_prefix('"') {
    Some(stripped[..stripped.find('"')?].to_string())
  } else { None }
}

// ─── TLS client config ────────────────────────────────────────────────────────

async fn build_tls_config(ca_cert_path: Option<&str>) -> Result<ClientConfig, Box<dyn std::error::Error + Send + Sync>> {
  let root_store = if let Some(path) = ca_cert_path {
    let mut reader = BufReader::new(File::open(path)?);
    let certs = rustls::pki_types::CertificateDer::pem_reader_iter(&mut reader).collect::<Result<Vec<_>, _>>()?;
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(certs);
    roots
  } else {
    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned())
  };
  Ok(ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth())
}

// ─── DNS fallback (system resolver → Cloudflare DoH) ─────────────────────────

async fn resolve_to_ip(hostname: &str) -> Option<String> {
  if let Ok(mut addrs) = tokio::net::lookup_host(format!("{}:0", hostname)).await {
    if let Some(addr) = addrs.next() { return Some(addr.ip().to_string()); }
  }
  let url = format!("https://cloudflare-dns.com/dns-query?name={}&type=A", hostname);
  let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
    .build(hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_only().enable_http1().build());
  let req = hyper::Request::builder().uri(&url).header("Accept", "application/dns-json")
    .body(http_body_util::Full::new(Bytes::new())).ok()?;
  let res = client.request(req).await.ok()?;
  let body = String::from_utf8_lossy(&res.into_body().collect().await.ok()?.to_bytes()).to_string();
  let answer_start = body.find("\"Answer\"")?;
  let after = &body[answer_start..];
  let mut pos = 0;
  while let Some(s) = after[pos..].find('{') {
    let abs = pos + s;
    let end = after[abs..].find('}').map(|e| abs + e + 1)?;
    let entry = &after[abs..end];
    if entry.contains("\"type\":1") || entry.contains("\"type\": 1") {
      if let Some(ip) = extract_string_field(entry, "data") { return Some(ip); }
    }
    pos = abs + 1;
    if pos >= after.len() { break; }
  }
  None
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args = Args::parse()?;
  tracing_subscriber::fmt()
    .with_max_level(if args.verbose { tracing::Level::DEBUG } else { tracing::Level::INFO })
    .init();
  let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());

  info!("Tunnel client v2 starting (tunnel_id={}, tls={})", args.tunnel_id, !args.no_tls);

  let default_port = if args.no_tls { "8779" } else { "8778" };
  let interval = Duration::from_secs(args.reconnect_interval);
  let mut last_proxy_addr: Option<String> = None;

  loop {
    // Re-fetch proxy address on every reconnect so address changes take effect without restart
    let proxy_addr = match fetch_tunnel_info(&args.api_url, &args.tunnel_id, &args.api_key).await {
      Ok(addr) => {
        let addr = if addr.contains(':') { addr } else { format!("{}:{}", addr, default_port) };
        if last_proxy_addr.as_deref() != Some(&addr) {
          info!("Proxy address: {}", addr);
          if args.no_tls {
            warn!("--no-tls: plain TCP. Ensure server has tunnel_port configured.");
          }
        }
        last_proxy_addr = Some(addr.clone());
        addr
      }
      Err(e) => {
        warn!("Failed to fetch proxy address: {} — retrying in {}s", e, args.reconnect_interval);
        tokio::time::sleep(interval).await;
        continue;
      }
    };

    if let Err(e) = run_session(&args, &proxy_addr).await {
      warn!("Session error: {}", e);
    }
    info!("Reconnecting in {} second(s)...", args.reconnect_interval);
    tokio::time::sleep(interval).await;
  }
}

// ─── Single session ───────────────────────────────────────────────────────────

async fn run_session(args: &Args, proxy_address: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  // Resolve hostname with DoH fallback
  let (host, port) = proxy_address.rfind(':')
    .map(|i| (&proxy_address[..i], &proxy_address[i+1..]))
    .unwrap_or((proxy_address, "8778"));
  let connect_addr = match resolve_to_ip(host).await {
    Some(ip) => { if ip != host { debug!("Resolved {} → {}", host, ip); } format!("{}:{}", ip, port) }
    None => proxy_address.to_string(),
  };

  let tcp = TcpStream::connect(&connect_addr).await?;

  if args.no_tls {
    info!("Connected (plain TCP) to {}", proxy_address);
    let (reader, writer) = tokio::io::split(tcp);
    run_session_inner(args, reader, writer).await
  } else {
    let sni = args.tls_server_name.as_deref().unwrap_or(host);
    let server_name = ServerName::try_from(sni.to_owned())
      .map_err(|e| format!("Invalid SNI '{}': {}", sni, e))?;
    let tls_config = build_tls_config(args.tls_ca_cert_path.as_deref()).await?;
    let tls_stream: TlsStream<TcpStream> = TlsConnector::from(Arc::new(tls_config)).connect(server_name, tcp).await?;
    info!("TLS connected to {}", proxy_address);
    let (reader, writer) = tokio::io::split(tls_stream);
    run_session_inner(args, reader, writer).await
  }
}

async fn run_session_inner<R, W>(
  args: &Args,
  mut reader: R,
  writer: W,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
  R: tokio::io::AsyncRead + Unpin + Send,
  W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
  let writer = Arc::new(Mutex::new(writer));

  // ── Send CONNECT frame ────────────────────────────────────────────────────
  {
    let payload = build_connect_payload(&args.tunnel_id, &args.api_key);
    let frame = Frame { frame_type: FrameType::Connect, stream_id: 0, flags: 0, payload };
    let mut w = writer.lock().await;
    write_frame(&mut *w, &frame).await?;
    w.flush().await?;
  }
  info!("Sent CONNECT (tunnel_id={})", args.tunnel_id);

  // ── Read CONFIG frame ─────────────────────────────────────────────────────
  let config_frame = read_frame(&mut reader).await?;
  if config_frame.frame_type != FrameType::Config {
    return Err(format!("Expected CONFIG, got {:?}", config_frame.frame_type).into());
  }
  let config = parse_config(&config_frame.payload)
    .ok_or("Failed to parse CONFIG payload")?;

  // Build domain_id → backend_host map
  let domain_map: Arc<HashMap<u16, String>> = Arc::new(
    config.domains.iter().map(|d| { info!("  domain_id={} → {}", d.domain_id, d.local_host); (d.domain_id, d.local_host.clone()) }).collect()
  );
  info!("CONFIG received: {} domain mappings", domain_map.len());

  // ── Body accumulator: stream_id → body_tx ─────────────────────────────────
  // Streams waiting for body DATA frames before their handler can run.
  let body_senders: Arc<Mutex<HashMap<u32, mpsc::Sender<Option<Bytes>>>>> = Arc::new(Mutex::new(HashMap::new()));

  // ── Reader loop ───────────────────────────────────────────────────────────
  loop {
    let frame = match read_frame(&mut reader).await {
      Ok(f) => f,
      Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
        info!("Tunnel server disconnected");
        return Ok(());
      }
      Err(e) => return Err(e.into()),
    };

    match frame.frame_type {
      FrameType::Request => {
        let req = match parse_request(&frame.payload) {
          Some(r) => r,
          None => { warn!("Bad REQUEST payload on stream {}", frame.stream_id); continue; }
        };

        let stream_id  = frame.stream_id;
        let has_body   = frame.has_flag(flags::HAS_BODY);
        let writer_c   = writer.clone();
        let domain_map = domain_map.clone();
        let body_senders_c = body_senders.clone();

        if has_body {
          // Create a channel for body DATA frames
          let (body_tx, body_rx) = mpsc::channel::<Option<Bytes>>(64);
          body_senders.lock().await.insert(stream_id, body_tx);
          tokio::spawn(handle_stream(stream_id, req, Some(body_rx), writer_c, domain_map, body_senders_c));
        } else {
          tokio::spawn(handle_stream(stream_id, req, None, writer_c, domain_map, body_senders_c));
        }
      }

      FrameType::Data => {
        let stream_id = frame.stream_id;
        let is_end    = frame.has_flag(flags::END_STREAM);
        let mut senders = body_senders.lock().await;
        if let Some(tx) = senders.get(&stream_id) {
          let _ = tx.send(Some(frame.payload)).await;
          if is_end {
            let _ = tx.send(None).await; // sentinel: end of body
            senders.remove(&stream_id);
          }
        }
      }

      FrameType::Reset => {
        let stream_id = frame.stream_id;
        let code = if frame.payload.len() >= 2 { u16::from_be_bytes([frame.payload[0], frame.payload[1]]) } else { 0 };
        debug!("RESET stream_id={} code={}", stream_id, code);
        body_senders.lock().await.remove(&stream_id);
      }

      FrameType::Ping => {
        let opaque = if frame.payload.len() >= 8 {
          let mut a = [0u8; 8]; a.copy_from_slice(&frame.payload[..8]); a
        } else { [0u8; 8] };
        let pong = Frame { frame_type: FrameType::Pong, stream_id: 0, flags: 0, payload: Bytes::copy_from_slice(&opaque) };
        let mut w = writer.lock().await;
        let _ = write_frame(&mut *w, &pong).await;
        let _ = w.flush().await;
      }

      FrameType::GoAway => {
        info!("GOAWAY received, closing session");
        return Ok(());
      }

      other => {
        debug!("Unexpected frame type from server: {:?}", other);
      }
    }
  }
}

// ─── Per-stream handler ───────────────────────────────────────────────────────

async fn handle_stream(
  stream_id: u32,
  req: RequestPayload,
  mut body_rx: Option<mpsc::Receiver<Option<Bytes>>>,
  writer: Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
  domain_map: Arc<HashMap<u16, String>>,
  body_senders: Arc<Mutex<HashMap<u32, mpsc::Sender<Option<Bytes>>>>>,
) {
  let backend = match domain_map.get(&req.domain_id) {
    Some(h) => h.clone(),
    None => {
      warn!("Unknown domain_id={} for stream {}", req.domain_id, stream_id);
      send_reset(&writer, stream_id, reset_codes::BACKEND_UNREACHABLE).await;
      return;
    }
  };

  // Collect body if expected
  let body: Option<Bytes> = if let Some(rx) = &mut body_rx {
    let mut buf = BytesMut::new();
    loop {
      match rx.recv().await {
        Some(Some(chunk)) => buf.extend_from_slice(&chunk),
        Some(None) | None => break,
      }
    }
    if buf.is_empty() { None } else { Some(buf.freeze()) }
  } else {
    None
  };

  debug!("stream {} → {} {} {}", stream_id, req.method, req.path, backend);

  // Make HTTP/1.1 request to local backend
  let result = forward_to_backend(stream_id, &req, body, &backend, &writer).await;
  if let Err(e) = result {
    warn!("stream {} backend error: {}", stream_id, e);
    body_senders.lock().await.remove(&stream_id);
    send_reset(&writer, stream_id, reset_codes::INTERNAL_ERROR).await;
  }
}

async fn forward_to_backend(
  stream_id: u32,
  req: &RequestPayload,
  body: Option<Bytes>,
  backend: &str,
  writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  let mut stream = TcpStream::connect(backend).await
    .map_err(|e| format!("Backend connect failed ({}): {}", backend, e))?;

  // Build HTTP/1.1 request — always send Connection: close so the backend closes
  // after the response, giving us a clean EOF for body detection.
  let mut req_bytes = Vec::new();
  req_bytes.extend_from_slice(format!("{} {} HTTP/1.1\r\n", req.method, req.path).as_bytes());
  for (name, value) in &req.headers {
    let lower = name.to_ascii_lowercase();
    if lower == "connection" || lower == "keep-alive" { continue; } // override below
    req_bytes.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
  }
  req_bytes.extend_from_slice(b"connection: close\r\n");
  if let Some(ref b) = body {
    let has_cl = req.headers.iter().any(|(n, _)| n.to_ascii_lowercase() == "content-length");
    if !has_cl {
      req_bytes.extend_from_slice(format!("content-length: {}\r\n", b.len()).as_bytes());
    }
  }
  req_bytes.extend_from_slice(b"\r\n");
  if let Some(ref b) = body { req_bytes.extend_from_slice(b); }

  stream.write_all(&req_bytes).await?;
  stream.flush().await?;

  // Read response headers
  let mut resp_buf = Vec::new();
  let mut tmp = [0u8; 4096];
  let header_end = loop {
    let n = stream.read(&mut tmp).await?;
    if n == 0 { break resp_buf.len(); }
    resp_buf.extend_from_slice(&tmp[..n]);
    if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
      break pos + 4;
    }
  };

  if header_end == 0 || resp_buf.is_empty() {
    return Err("Backend closed without sending response headers".into());
  }

  // Parse status line + headers
  let header_str = std::str::from_utf8(&resp_buf[..header_end])
    .map_err(|_| "Invalid UTF-8 in response headers")?;
  let mut lines = header_str.lines();
  let status_line = lines.next().ok_or("Missing status line")?;
  let status_code: u16 = status_line.split_whitespace().nth(1)
    .ok_or("Missing status code")?
    .parse().map_err(|_| "Invalid status code")?;

  let mut resp_headers: Vec<(String, String)> = Vec::new();
  let mut content_length: Option<usize> = None;
  let mut is_chunked = false;
  for line in lines {
    if line.is_empty() { break; }
    if let Some((name, value)) = line.split_once(':') {
      let name_lc = name.trim().to_ascii_lowercase();
      let val = value.trim().to_string();
      if name_lc == "content-length" { content_length = val.parse().ok(); }
      if name_lc == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") { is_chunked = true; }
      if name_lc != "transfer-encoding" { // strip TE — we'll dechunk for client
        resp_headers.push((name.trim().to_string(), val));
      }
    }
  }

  // Status codes that carry no body per RFC 7230 §3.3
  let no_body = (100..200).contains(&status_code) || status_code == 204 || status_code == 304
    || req.method.eq_ignore_ascii_case("HEAD");

  // Body already partially read (bytes after header_end)
  let mut body_buf: Vec<u8> = if no_body { Vec::new() } else { resp_buf[header_end..].to_vec() };

  if !no_body {
    if let Some(cl) = content_length {
      while body_buf.len() < cl {
        let n = stream.read(&mut tmp).await?;
        if n == 0 { break; }
        body_buf.extend_from_slice(&tmp[..n]);
      }
      body_buf.truncate(cl);
    } else if is_chunked {
      loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 { break; }
        body_buf.extend_from_slice(&tmp[..n]);
        if body_buf.windows(5).any(|w| w == b"0\r\n\r\n") { break; }
      }
      body_buf = dechunk(&body_buf);
    } else {
      // No Content-Length, not chunked — read until EOF (Connection: close ensures this)
      // Use a short deadline to avoid hanging on responses that never close
      let read_fut = async {
        loop {
          let n = stream.read(&mut tmp).await?;
          if n == 0 { break; }
          body_buf.extend_from_slice(&tmp[..n]);
        }
        Ok::<_, std::io::Error>(())
      };
      let _ = tokio::time::timeout(std::time::Duration::from_secs(10), read_fut).await;
    }
  }

  // Update Content-Length header to match actual body
  if let Some(pos) = resp_headers.iter().position(|(n, _)| n.to_ascii_lowercase() == "content-length") {
    resp_headers[pos].1 = body_buf.len().to_string();
  } else {
    resp_headers.push(("content-length".to_string(), body_buf.len().to_string()));
  }

  // Send RESPONSE frame
  let resp_flags = if body_buf.is_empty() { 0 } else { flags::RESPONSE_HAS_BODY };
  let resp_payload = build_response_payload(status_code, &resp_headers);
  let resp_frame = Frame { frame_type: FrameType::Response, stream_id, flags: resp_flags, payload: resp_payload };
  {
    let mut w = writer.lock().await;
    write_frame(&mut *w, &resp_frame).await?;
    w.flush().await?;
  }

  // Send DATA frames (chunk at 64 KiB)
  if !body_buf.is_empty() {
    const CHUNK: usize = 65536;
    let total = body_buf.len();
    let mut sent = 0;
    while sent < total {
      let end = (sent + CHUNK).min(total);
      let is_last = end == total;
      let data_flags = if is_last { flags::END_STREAM } else { 0 };
      let chunk = Bytes::copy_from_slice(&body_buf[sent..end]);
      let data_frame = Frame { frame_type: FrameType::Data, stream_id, flags: data_flags, payload: chunk };
      let mut w = writer.lock().await;
      write_frame(&mut *w, &data_frame).await?;
      w.flush().await?;
      sent = end;
    }
  }

  debug!("stream {} done: {} {}", stream_id, status_code, backend);
  Ok(())
}

async fn send_reset(writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>, stream_id: u32, error_code: u16) {
  let mut payload = BytesMut::new();
  payload.put_u16(error_code);
  let frame = Frame { frame_type: FrameType::Reset, stream_id, flags: 0, payload: payload.freeze() };
  let mut w = writer.lock().await;
  let _ = write_frame(&mut *w, &frame).await;
  let _ = w.flush().await;
}

// ─── Chunked transfer decoding ────────────────────────────────────────────────

fn dechunk(data: &[u8]) -> Vec<u8> {
  let mut result = Vec::new();
  let mut pos = 0;
  while pos < data.len() {
    let line_end = match data[pos..].windows(2).position(|w| w == b"\r\n") {
      Some(p) => pos + p,
      None => break,
    };
    let size_str = std::str::from_utf8(&data[pos..line_end]).unwrap_or("0");
    let chunk_size = usize::from_str_radix(size_str.trim(), 16).unwrap_or(0);
    if chunk_size == 0 { break; }
    pos = line_end + 2;
    if pos + chunk_size > data.len() { break; }
    result.extend_from_slice(&data[pos..pos + chunk_size]);
    pos += chunk_size + 2; // skip trailing CRLF
  }
  result
}
