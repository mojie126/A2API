//! Cloudflare WARP 用户态出口池
//!
//! 参考 opencode2api 的注册、出口与池设计：注册设备后通过 UDP 承载 WireGuard，
//! 在进程内的用户态网络栈里终结 TCP，不需要 TUN、root 或系统路由
//!
//! 当前先落地设备注册与配置解析，隧道、网络栈与 SOCKS 入口在后续模块补齐

pub mod netstack;
pub mod pool;
pub mod register;
pub mod socks;
pub mod tunnel;
