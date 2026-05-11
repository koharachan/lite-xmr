# lite-xmr

轻量级 Monero (XMR) CPU 矿工。基于 XMRig，用 Rust 重写了 CLI 入口和网络层。

## 特性

- **RandomX** 算法支持，保留 XMRig C++ 高性能挖矿后端
- **纯 Rust TLS** (rustls + webpki-roots)，无需链接 OpenSSL
- **完全无抽水**，删除了 XMRig 默认的 1% dev fee
- 异步 Stratum 协议客户端 (tokio)

## 快速开始

```bash
cargo build --release
./target/release/lite-xmr -o pool.supportxmr.com:3333 -u <WALLET_ADDRESS>
```

## 命令行选项

```
用法: lite-xmr [选项]

选项:
  -o, --url <HOST:PORT>    矿池地址
  -u, --user <ADDRESS>     钱包地址或用户名
  -p, --pass <STRING>      矿池密码 (默认: x)
  -t, --threads <N>        挖矿线程数 (0 = 自动检测)
      --tls                使用 TLS 连接矿池
      --config <PATH>      配置文件路径
      --log-level <LEVEL>  日志级别 (默认: info)
      --api-bind <ADDR>    HTTP API 监听地址
      --keepalive          保持连接活跃 (不挖矿)
  -h, --help               显示帮助信息
  -v, --version            显示版本号
```

## 构建

需要 Rust 工具链 (1.75+) 和 C++ 编译器。

```bash
git clone https://github.com/your/lite-xmr
cd lite-xmr
cargo build --release
```

<br />

<br />

## 项目结构

```
├── crates/
│   ├── lite-xmr-cli/       # Rust 二进制入口
│   ├── lite-xmr-core/      # 共享类型、配置、CPU 检测
│   ├── lite-xmr-miner/     # 挖矿核心 (randomx-rs)
│   └── lite-xmr-stratum/   # Stratum 协议客户端
├── src/                    # XMRig C++ 后端 (RandomX 核心)
├── cmake/                  # CMake 模块
├── doc/                    # 文档
└── Cargo.toml              # Rust 工作区
```

## 许可证

GPL-3.0
