# 个人 fork 构建与 GHCR

本说明面向个人 fork 维护者，官方安装流程仍以 `docs/install.md` 为准。个人 fork 的 `performance-build` 工作流只允许在 Actions 页面手动触发，不响应标签推送。

## 构建产物

- `windows-nsis`：Windows 桌面端安装包
- `windows-server-amd64`：Windows 服务端压缩包与校验文件
- `ghcr.io/<所有者>/a2api:latest`：`linux/amd64` 容器镜像

手动运行工作流后，在对应运行的 Artifacts 区域下载 Windows 产物。每次手动构建都会覆盖 `latest`。

## 使用镜像

首次推送后，到 GitHub 仓库的 Packages 页面确认 `a2api` 容器包已关联到该 fork。GHCR 新包可能默认私有，需要按实际使用范围调整可见性。

```bash
docker pull ghcr.io/mojie126/a2api:latest
docker run -d --name agent2api --restart unless-stopped \
  -p 3065:3065 -v ./data:/data \
  ghcr.io/mojie126/a2api:latest
```

## 群晖等旧版 Docker

镜像只支持 `linux/amd64`，适用于 x86_64 架构的群晖设备。群晖 Docker 20.10 的普通 `docker pull` 不需要在本机构建镜像。

若群晖无法直接访问 `ghcr.io`，请先确认网络与代理可达，再将镜像地址改为能够访问的镜像代理前缀。不要在仓库中写入访问令牌或代理凭据。
