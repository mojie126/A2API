//! Cloudflare WARP 用户态出口池
//!
//! 参考 opencode2api 的注册、出口与池设计：注册设备后通过 UDP 承载 WireGuard，
//! 在进程内的用户态网络栈里终结 TCP，不需要 TUN、root 或系统路由
//!
//! `settings` 保存期望出口数，`pool` 按它注册设备，`register` / `tunnel` /
//! `netstack` / `socks` 逐层把设备变成可用的本地 SOCKS5 出口

pub mod netstack;
pub mod pool;
pub mod register;
pub mod settings;
pub mod socks;
pub mod tunnel;
