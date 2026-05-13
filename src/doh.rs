use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

const DNS_RESOLVERS: &[&str] = &[
    "8.8.8.8:53",
    "8.8.4.4:53",
    "1.1.1.1:53",
    "1.1.4.4:53",
    "223.5.5.5:53",
    "223.6.6.6:53",
    "119.29.29.29:53",
    "114.114.114.114:53",
    "[2402:4e00::]:53",
    "[2400:3200::1]:53",
    "[2400:3200:baba::1]:53",
];

const DOH_SERVERS: &[&str] = &[
    "https://dns.alidns.com/dns-query",
    "https://doh.pub/dns-query",
    "https://doh.360.cn/dns-query",
    "https://dns.twnic.tw/dns-query",
    "https://dns.google/dns-query",
    "https://cloudflare-dns.com/dns-query",
    "https://doh.sb/dns-query",
];

fn build_dns_query(hostname: &str, qtype: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&0x1234u16.to_be_bytes());
    buf.extend_from_slice(&0x0100u16.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf.extend_from_slice(&[0u8; 6]);
    for label in hostname.split('.') {
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0);
    buf.extend_from_slice(&qtype.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf
}

fn skip_name(data: &[u8], mut pos: usize) -> usize {
    loop {
        if pos >= data.len() || data[pos] == 0 {
            return pos + 1;
        }
        if data[pos] & 0xC0 == 0xC0 {
            return pos + 2;
        }
        pos += data[pos] as usize + 1;
    }
}

fn parse_dns_response(data: &[u8]) -> Vec<SocketAddr> {
    if data.len() < 12 {
        return vec![];
    }
    let qdcount = u16::from_be_bytes([data[4], data[5]]) as usize;
    let ancount = u16::from_be_bytes([data[6], data[7]]) as usize;
    let mut pos = 12;
    for _ in 0..qdcount {
        pos = skip_name(data, pos);
        if pos + 4 > data.len() {
            return vec![];
        }
        pos += 4;
    }
    let mut results = Vec::new();
    for _ in 0..ancount {
        if pos + 10 > data.len() {
            break;
        }
        pos = skip_name(data, pos);
        if pos + 10 > data.len() {
            break;
        }
        let atype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlength > data.len() {
            break;
        }
        match atype {
            1 if rdlength == 4 => {
                let ip = std::net::Ipv4Addr::new(data[pos], data[pos + 1], data[pos + 2], data[pos + 3]);
                results.push(SocketAddr::new(ip.into(), 0));
            }
            28 if rdlength == 16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&data[pos..pos + 16]);
                results.push(SocketAddr::new(std::net::Ipv6Addr::from(octets).into(), 0));
            }
            _ => {}
        }
        pos += rdlength;
    }
    results
}

fn base64url_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        s.push(T[((triple >> 18) & 0x3F) as usize] as char);
        s.push(T[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            s.push(T[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            s.push(T[(triple & 0x3F) as usize] as char);
        }
    }
    s
}

fn ping_dns_resolver(addr: &str, timeout: Duration) -> Option<Duration> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    let start = Instant::now();
    let query = build_dns_query("localhost", TYPE_A);
    if socket.send_to(&query, addr).is_err() {
        return None;
    }
    let mut buf = [0u8; 512];
    match socket.recv(&mut buf) {
        Ok(_) => Some(start.elapsed()),
        Err(_) => None,
    }
}

fn doh_query(server: &str, hostname: &str, qtype: u16) -> Vec<SocketAddr> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build(),
    );
    let query = build_dns_query(hostname, qtype);
    let encoded = base64url_encode(&query);
    let url = format!("{}?dns={}", server, encoded);
    let resp = match agent
        .get(&url)
        .header("Accept", "application/dns-message")
        .call()
    {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let body = match resp.into_body().read_to_vec() {
        Ok(b) => b,
        Err(_) => return vec![],
    };
    parse_dns_response(&body)
}

fn bench_doh_server(server: &str) -> Option<Duration> {
    let start = Instant::now();
    let result = doh_query(server, "example.com", TYPE_A);
    if result.is_empty() {
        return None;
    }
    Some(start.elapsed())
}

pub fn resolve(hostname: &str, port: u16) -> Option<SocketAddr> {
    info!("DOH: 正在通过 {} 个 DNS 解析器测试延迟...", DNS_RESOLVERS.len());
    let ping_timeout = Duration::from_secs(2);

    let mut resolver_times: Vec<(&str, Duration)> = DNS_RESOLVERS
        .iter()
        .filter_map(|&r| {
            ping_dns_resolver(r, ping_timeout).map(|d| {
                debug!("DOH: 解析器 {} 延迟 {}ms", r, d.as_millis());
                (r, d)
            })
        })
        .collect();

    resolver_times.sort_by_key(|(_, d)| *d);
    resolver_times.truncate(3);
    info!("DOH: 最快的3个 DNS 解析器: {:?}", resolver_times.iter().map(|(a, d)| format!("{}({}ms)", a, d.as_millis())).collect::<Vec<_>>());

    info!("DOH: 正在测试 {} 个 DOH 服务器...", DOH_SERVERS.len());
    let mut doh_times: Vec<(&str, Duration)> = DOH_SERVERS
        .iter()
        .filter_map(|&s| {
            bench_doh_server(s).map(|d| {
                info!("DOH: {} 延迟 {}ms", s, d.as_millis());
                (s, d)
            })
        })
        .collect();

    if doh_times.is_empty() {
        warn!("DOH: 没有可用的 DOH 服务器");
        return None;
    }

    doh_times.sort_by_key(|(_, d)| *d);
    doh_times.truncate(2);
    info!("DOH: 最快的2个 DOH 服务器: {:?}", doh_times.iter().map(|(a, d)| format!("{}({}ms)", a, d.as_millis())).collect::<Vec<_>>());

    info!("DOH: 正在通过 DOH 解析 {} 的 A/AAAA 记录...", hostname);
    let mut addrs: Vec<SocketAddr> = Vec::new();
    for &(server, _) in &doh_times {
        for a in doh_query(server, hostname, TYPE_A) {
            let a = SocketAddr::new(a.ip(), port);
            debug!("DOH: {} → A {}", server, a);
            addrs.push(a);
        }
        for a in doh_query(server, hostname, TYPE_AAAA) {
            let a = SocketAddr::new(a.ip(), port);
            debug!("DOH: {} → AAAA {}", server, a);
            addrs.push(a);
        }
    }

    if addrs.is_empty() {
        warn!("DOH: 未能解析 {}", hostname);
        return None;
    }

    info!("DOH: 解析到 {} 个地址，正在测试 TCP 连接延迟...", addrs.len());
    let mut addr_times: Vec<(SocketAddr, Duration)> = Vec::new();
    for &addr in &addrs {
        let start = Instant::now();
        match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
            Ok(_) => {
                let d = start.elapsed();
                debug!("DOH: TCP {} 延迟 {}ms", addr, d.as_millis());
                addr_times.push((addr, d));
            }
            Err(e) => {
                debug!("DOH: TCP {} 连接失败: {}", addr, e);
            }
        }
    }

    addr_times.sort_by_key(|(_, d)| *d);

    if let Some((addr, d)) = addr_times.first() {
        info!("DOH: 选择最快地址 {} ({}ms)", addr, d.as_millis());
        Some(*addr)
    } else {
        warn!("DOH: 所有地址连接失败");
        None
    }
}
