# 白桦：属于开发者的沟通工具

[![Author: Gavin Zheng](https://img.shields.io/badge/Author-Gavin_Zheng-f2f28d)](https://github.com/GavZheng)
![Language: Rust](https://img.shields.io/badge/Language-Rust-orange)
![Version: 0.1.5](https://img.shields.io/badge/Version-0.1.5-blue)
![License: Apache v2](https://img.shields.io/badge/License-Apache%20v2-green)
![Github Stars](https://img.shields.io/github/stars/Binder-organize/Baihua-Server?style=flat&color=red)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-3.0-4baaaa.svg)](CODE_OF_CONDUCT_zh-CN.md)

[English](../../README.md) | [简体中文](README_zh-CN.md)

白桦是一个由**Rust**编写的，为开发者定制的团队沟通工具，用于帮助开发者快速完成各种任务。

---

## 目录
- [目录](#目录)
- [为什么我们需要白桦](#为什么我们需要白桦)
- [快速启动](#快速启动)
- [如何参与白桦的开发](#如何参与白桦的开发)
- [特别致谢](#特别致谢)
- [贡献者](#贡献者)
- [FAQ](#faq)
- [许可证](#许可证)

---

## 为什么我们需要白桦
在今天的开发工作中，我们往往需要频繁切换多个工具和平台：例如，在终端操作 Git、在浏览器查看项目的 Issues、在另一个面板管理 CI/CD，以及使用表格或独立工具跟踪进度……这种碎片化的工作流不仅降低效率，更打断了开发者最宝贵的“心流”状态。

白桦 正是为了终结这种碎片化体验而生。它是一个针对开发者定制的团队沟通工具，深度整合了你日常使用的 Git、GitHub/GitLab、依赖管理、构建系统等工具，将它们统一到一个连贯的工作流中。

---

## 快速启动

### 前置条件

- **Docker**（用于 PostgreSQL 数据库）
- **Rust 工具链**（stable，包含 rustfmt 和 clippy）
- **Python 3.10+**，并执行 `pip install -r tests/requirements.txt`（用于测试）

### 开发模式

```bash
# 1. 配置环境变量
cp .env.example .env

# 2. 通过 Docker 启动 PostgreSQL
docker compose up -d database

# 3. 启动服务端（首次启动自动执行数据库迁移）
cargo run
```

服务端运行在 `http://localhost:2424`，终端中会启动交互式控制台——输入 `help` 查看命令。

如需运行完整的测试套件（编译 + 数据库 + 服务端 + pytest）：

```bash
python3 tests/run_tests.py
```

### 生产部署

使用 Docker Compose 部署整个服务栈：

```bash
# 1. 准备生产环境变量
cp .env.production.example .env.production
# 然后编辑 .env.production，填入你的生产配置。

# 2. 构建并启动所有服务（首次构建可能需要 10-15 分钟）
docker compose --env-file .env.production --profile production up -d --build
```

> 首次构建需要在 Docker 里从零下载并编译所有 Rust 依赖。
> 后续构建耗时取决于哪些构建层仍可使用缓存。
> 如需查看构建进度，去掉 `-d` 参数：`docker compose --env-file .env.production --profile production up --build`。

公开部署前，请先检查这些事项：

- 部署主机已经安装 Docker 和 Docker Compose。
- 将 `BAIHUA_DOMAIN` 指向部署主机。
- 允许外部访问 80 和 2424 端口。80 端口仅用于证书验证和跳转；客户端通过 2424 端口建立加密连接。
- 将 `POSTGRES_PASSWORD` 和 `JWT_SECRET` 替换为强随机值。
- 不要将 `.env.production` 提交到版本控制。
- 升级前备份数据库和头像数据卷。

启动后包含数据库、服务端和加密公开入口：

| 服务           | 容器名               | 端口       |
|--------------|-------------------|----------|
| **database** | `baihua-database` | 2423（仅本机） |
| **server**   | `baihua-server`   | 仅限 Docker 内部网络 |
| **reverse proxy** | `caddy` | 80 用于证书验证和跳转；2424 用于加密连接 |

数据库端口只绑定到部署主机的本地地址，不会暴露到互联网。服务端没有主机端口映射，Caddy 通过 Docker 内部网络访问服务端。客户端使用 `https://BAIHUA_DOMAIN:2424` 连接；80 端口仅用于证书验证，并将访问者跳转到加密地址。WebSocket 客户端使用 `wss://BAIHUA_DOMAIN:2424/websocket` 连接。数据库记录保存在 `baihua-postgres-data` 卷，上传的头像保存在 `baihua-avatar-data` 卷。

生产环境中，服务端默认拒绝没有经过加密连接的请求。Caddy 会自动添加加密标记，并忽略客户端自行提供的同名标记。不要把服务端端口直接暴露到公网。如果使用其他反向代理，请确保它只在加密连接时将 `X-Forwarded-Proto` 设为 `https`。容器内部运行的健康检查不受此限制。默认开启这项检查；只有明确将 `.env.production` 中的 `BAIHUA_REQUIRE_ENCRYPTED_TRANSPORT` 设为 `false` 才会关闭。开发环境不执行这项检查。

如需公开访问，在 `.env.production` 中将 `BAIHUA_DOMAIN` 设为指向此主机的域名，并允许外部连接端口 80 和 2424。Caddy 会随生产配置自动启动、申请并续期证书，并将未加密请求跳转到 2424 端口。服务端默认强制加密；只有确实需要接受未加密请求时，才将 `BAIHUA_REQUIRE_ENCRYPTED_TRANSPORT` 设为 `false`：

```bash
docker compose --env-file .env.production --profile production up -d --build
```

反向代理负责管理证书和转发 WebSocket 连接，证书状态保存在 `baihua-caddy-data` 卷。服务端和数据库端口不会暴露到互联网。

如果你已经使用其他反向代理、负载均衡或入口控制器，请从 Compose 部署中移除 `reverse-proxy` 服务，并让现有入口代理通过 Docker 内部网络转发到服务端。同时确认它提供加密连接并转发 WebSocket 升级请求。

服务端依赖数据库健康检查才启动，并自带 Docker HEALTHCHECK（`GET /health`）。查看日志：

```bash
docker compose --env-file .env.production --profile production logs -f
```

停止服务并保留数据库、头像及证书数据卷：

```bash
docker compose --env-file .env.production --profile production down
```

### 升级已有部署

先备份生产数据，再更新仓库并重新构建容器：

```bash
git pull
docker compose --env-file .env.production --profile production up -d --build
```

服务端启动时会自动执行数据库迁移。升级后检查服务健康状态并查看最近日志：

```bash
docker compose --env-file .env.production --profile production exec -T server curl -fsS http://127.0.0.1:2424/health
docker compose --env-file .env.production --profile production logs --tail=100
```

### 备份与恢复

在维护窗口中先停止服务端，再一起备份数据库与头像，避免备份期间的头像上传造成数据不一致。备份结束后重新启动服务端；命令失败时也要重新启动。将备份目录复制到另一台主机的受保护存储中：

```bash
umask 077
backup_directory="backups/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$backup_directory"
docker compose --env-file .env.production --profile production stop server
docker compose --env-file .env.production exec -T database sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc' > "$backup_directory/database.dump"
docker run --rm --mount type=volume,src=baihua-avatar-data,dst=/avatars,readonly --mount "type=bind,src=$PWD/$backup_directory,dst=/backups" alpine tar -C /avatars -cf /backups/avatars.tar .
docker compose --env-file .env.production --profile production start server
```

还应将 Caddy 的证书状态备份到同一个目录：

```bash
docker run --rm --mount type=volume,src=baihua-caddy-data,dst=/caddy-data,readonly --mount "type=bind,src=$PWD/$backup_directory,dst=/backups" alpine tar -C /caddy-data -cf /backups/caddy-data.tar .
```

恢复时使用全新的空数据库与空数据卷。选择已有备份目录，先只启动数据库，再恢复数据库和头像，最后启动服务端：

```bash
backup_directory=backups/SELECTED_BACKUP_DIRECTORY
docker compose --env-file .env.production up -d --wait database
docker compose --env-file .env.production exec -T database sh -c 'pg_restore --exit-on-error --no-owner -U "$POSTGRES_USER" -d "$POSTGRES_DB"' < "$backup_directory/database.dump"
docker run --rm --mount type=volume,src=baihua-avatar-data,dst=/avatars --mount "type=bind,src=$PWD/$backup_directory,dst=/backups,readonly" alpine tar -C /avatars -xf /backups/avatars.tar
docker compose --env-file .env.production --profile production up -d server
```

将 `caddy-data.tar` 恢复到空的证书数据卷，然后启动公开入口：

```bash
docker run --rm --mount type=volume,src=baihua-caddy-data,dst=/caddy-data --mount "type=bind,src=$PWD/$backup_directory,dst=/backups,readonly" alpine tar -C /caddy-data -xf /backups/caddy-data.tar
docker compose --env-file .env.production --profile production up -d reverse-proxy
```

每次恢复演练后，检查服务健康状态、用已有账号登录，并获取一次已上传头像。恢复到已有部署前先停止服务端，不要覆盖运行中的数据库。安全保存 `.env.production` 和自定义配置。

---

## 如何参与白桦的开发
白桦是一个开源项目，我们热忱欢迎并高度期待来自全球各地的开发者能够加入并参与到白桦的开发进程中来。
你可以通过以下方式参与白桦的开发：
1. 提交漏洞报告和功能建议：如果你在使用白桦的过程中发现了漏洞或者有任何功能建议，请参阅[白桦安全指南](SECURITY_zh-CN.md)。
2. 贡献代码：如果你有能力并且愿意为白桦贡献代码，请参阅[白桦贡献者指南](CONTRIBUTING_zh-CN.md)文件。

---

## 特别致谢
对以下为白桦做出突出贡献的人员表示真挚地感谢（以首字母为序）：
- [Bob](https://github.com/ChepleBob30)：为白桦的开发做出了非常多非代码的贡献，是白桦的第一位用户。

---

## 贡献者
<a href="https://github.com/Binder-organize/Baihua-Server/contributors">
  <img src="https://contrib.rocks/image?repo=Binder-organize/Baihua-Server" alt="Contributors"/>
</a>

---

## FAQ
**Q1：Gavin 使用什么开发设备？**  
**A1：** MacBook Air M1。

**Q2：更多关于 Gavin 的信息？**  
**A2：** 你可以访问[Gavin的Github主页](https://github.com/GavZheng)。

**Q3：为什么选择Rust？**  
**A3：** Rust凭借其卓越的跨平台能力、内存安全特性和高效执行性能，在综合评估Python/C++/C等候选语言后，被确认为满足项目需求的最佳技术选型。

---

## 许可证
[Apache v2](../../LICENSE), Copyright 2026 Gavin Zheng.
