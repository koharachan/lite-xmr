# lite-xmr

<div align="center">

![Version](https://img.shields.io/badge/version-1.0.0-blue)
![Rust](https://img.shields.io/badge/Rust-1.75+-orange.svg)
![License](https://img.shields.io/badge/license-GPL--3.0-green)

**轻量级、高性能 Monero (XMR) CPU 挖矿工具**

Rust + C/C++ 实现，无第三方 C/C++ 依赖，无需手动下载依赖，静态编译，开箱即用。

</div>

---

## 特性

- **Pure Rust** - 100% Rust 实现，无任何 C/C++ FFI 依赖
- **轻量高效** - 专为 x86_64 架构优化，内存占用低
- **零抽水** - 开源透明，无开发者的秘密捐赠算力
- **静态编译** - 单二进制文件，便于部署
- **跨平台** - 支持 Linux、macOS、Windows

## 性能

| 配置 | 算力 (H/s) |
|------|-----------|
| AMD Ryzen 7 3700X | ~10,000 |
| Intel Core i7-6700 | ~7,500 |
| AMD Ryzen Threadripper 2950X | ~15,000 |

> 实际算力取决于系统配置、内存速度和 CPU 缓存。

## 系统要求

- **CPU**: x86_64 处理器，支持 AES-NI 和 AVX2
- **内存**: 最少 2GB (RandomX 算法需求)
- **操作系统**: Linux / macOS / Windows
- **Rust**: 1.75+

## 安装

### 从源码编译

```bash
# 克隆仓库
git clone https://cnb.cool/rainchan/lite-xmr.git
cd lite-xmr

# 编译 release 版本
cargo build --release

# 二进制文件位于 target/release/lite-xmr
```

### 预编译版本

前往 [Releases](https://cnb.cool/rainchan/lite-xmr/releases) 页面下载对应平台的预编译二进制文件。

## 使用

### 基础用法

```bash
./lite-xmr -o pool.supportxmr.com:3333 -u <YOUR_XMR_WALLET>
```

### 命令行参数

| 参数 | 描述 | 示例 |
|------|------|------|
| `-o, --pool` | 矿池地址 | `-o pool.supportxmr.com:3333` |
| `-u, --user` | 钱包地址或用户名 | `-u 4An3...7kQ9` |
| `-p, --pass` | 矿池密码 (可选) | `-p x` |
| `-t, --threads` | 挖矿线程数 (可选) | `-t 8` |
| `-r, --retries` | 连接重试次数 (可选) | `-r 5` |
| `-R, --retry-pause` | 重试间隔秒数 (可选) | `-R 10` |
| `--tls` | 启用 TLS 连接 (可选) | `--tls` |
| `-v, --verbose` | 输出详细日志 | `-v` |
| `-V, --version` | 显示版本信息 | `-V` |
| `-h, --help` | 显示帮助信息 | `-h` |

### 配置示例

```bash
# 使用 8 线程连接到 SupportXMR 矿池
./lite-xmr -o pool.supportxmr.com:3333 -u YOUR_WALLET -t 8

# 使用 TLS 连接到矿池
./lite-xmr -o pool.supportxmr.com:443 -u YOUR_WALLET --tls -t 8

# 自定义重试参数
./lite-xmr -o pool.supportxmr.com:3333 -u YOUR_WALLET -r 10 -R 30 -t 16
```

## 支持的矿池

- [SupportXMR](https://supportxmr.com/)
- [Monero Ocean](https://moneroocean.stream/)
- [Nanopool](https://xmr.nanopool.org/)
- [MineXMR](https://minexmr.com/)

## 工作原理

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   Wallet    │────▶│   Pool      │────▶│   Worker    │
│   (You)     │◀────│  Stratum    │◀────│  (lite-xmr) │
└─────────────┘     └─────────────┘     └─────────────┘
     │                                        │
     │  1. Connect & Subscribe                │
     │  2. Receive Jobs ──────────────────────▶│
     │                                        │
     │  3. Submit Share ◀──────────────────────│
     │  (Proof of Work)                       │
     └────────────────────────────────────────┘
```

lite-xmr 通过 Stratum 协议与矿池通信，接收挖矿任务并使用 RandomX 算法执行工作量证明。

## 故障排除

### 常见问题

**Q: 提示 "CPU not supported"**
> 你的 CPU 可能不支持 RandomX 所需指令集 (AES-NI, AVX2)。RandomX 主要面向现代 x86_64 处理器。

**Q: 算力很低**
> 尝试增加线程数，确保使用 release 模式编译，并检查是否启用了 LTO。

**Q: 连接失败**
> 检查矿池地址和端口是否正确，确认网络连接正常，尝试添加 `-v` 参数查看详细日志。

**Q: Windows 报错缺少 DLL**
> 使用静态编译版本或安装 Visual C++ Redistributable。

### 调试

启用详细日志模式：

```bash
./lite-xmr -o pool.supportxmr.com:3333 -u YOUR_WALLET -v
```

## 开发

### 项目结构

```
src/
├── main.rs           # 程序入口
├── config.rs         # 配置解析
├── cpu.rs            # CPU 检测
├── job.rs            # Stratum 任务处理
├── stats.rs          # 算力统计
├── error.rs          # 错误类型
├── miner/            # 挖矿核心
│   ├── mod.rs
│   └── worker.rs     # RandomX worker
└── stratum/          # Stratum 协议
    ├── mod.rs
    ├── client.rs     # Stratum 客户端
    └── transport.rs  # TCP/TLS 传输
```

### 构建优化

项目已配置以下 release 优化：

- `lto = true` - 链接时优化
- `opt-level = 3` - 最高优化级别
- `codegen-units = 1` - 单一代码生成单元
- `strip = true` - 剥离调试符号

## 性能优化建议

1. **关闭超线程** - 在 BIOS 中禁用 CPU 超线程可提升约 10% 算力
2. **使用 fast memory** - RandomX 对内存带宽敏感
3. **NUMA 亲和性** - 多路服务器上使用 `hwloc` 绑定 CPU
4. **编译优化** - 确保使用 `--release` 模式

## 贡献

欢迎提交 Issue 和 Pull Request！

## 许可证

本项目采用 [GPL-3.0](LICENSE) 许可证开源。

## 免责声明

本软件仅供学习交流使用，请遵守当地法律法规。挖矿行为可能涉及较高的电力消耗和硬件磨损。
