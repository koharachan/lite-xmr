# lite-xmr

纯 Rust 轻量级 Monero (XMR) CPU 矿工。x64 专用，无抽水，无 GPU，无 ARM。

## 架构

```
src/
├── main.rs            # 入口：版本/摘要/事件循环
├── config.rs          # CLI 参数 + TOML 配置合并
├── cpu.rs             # CPU 检测 (cpuid + hwlocality) + 内存信息 (sysinfo)
├── job.rs             # Stratum Job/SubmitResult + nonce 偏移量 + 难度解码
├── stats.rs           # 算力统计
├── error.rs           # 统一错误类型
├── miner/
│   ├── mod.rs
│   └── worker.rs      # 多线程 RandomX 挖矿 (randomx-rs)
└── stratum/
    ├── mod.rs
    ├── client.rs      # 异步 Stratum 协议 (connect/subscribe/authorize/submit)
    └── transport.rs   # TCP/TLS 传输 (rustls)
```

## 依赖

| 用途 | Crate | 替代 |
|---|---|---|
| 异步运行时 | tokio | libuv |
| TLS | rustls + webpki-roots | OpenSSL |
| CPU 拓扑/缓存 | hwlocality (vendored) | hwloc |
| 系统信息 | sysinfo | - |
| CPU 特性 | raw-cpuid | - |
| RandomX | randomx-rs | XMRig C++ RandomX |
| 日志 | tracing + tracing-subscriber | - |

全部 Rust，无 C/C++ 构建依赖，纯静态编译。

## 构建

需要 Rust ≥ 1.75。

```bash
cargo build --release
```

## 运行

```bash
lite-xmr -o pool.supportxmr.com:3333 -u <WALLET>
```

## 命令行

```
  -o, --url <HOST:PORT>    矿池地址
  -u, --user <ADDRESS>     钱包地址
  -p, --pass <STRING>      矿池密码 (默认: x)
  -t, --threads <N>        线程数 (0 = 自动)
      --tls                TLS 连接
      --config <PATH>      配置文件 (TOML)
      --log-level <LEVEL>  日志级别 (默认: info)
      --api-bind <ADDR>    HTTP API 监听地址
      --keepalive          保持连接
  -h, --help
  -v, --version
```

## 许可证

GPL-3.0
