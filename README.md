# Baihua: The Communication Tool for Developers

[![Author: Gavin Zheng](https://img.shields.io/badge/Author-Gavin_Zheng-f2f28d)](https://github.com/GavZheng)
![Language: Rust](https://img.shields.io/badge/Language-Rust-orange)
![Version: 0.1.4](https://img.shields.io/badge/Version-0.1.4-blue)
![License: Apache v2](https://img.shields.io/badge/License-Apache%20v2-green)
![Github Stars](https://img.shields.io/github/stars/Binder-organize/Baihua-Server?style=flat&color=red)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-3.0-4baaaa.svg)](CODE_OF_CONDUCT.md)
[![Docs](https://img.shields.io/badge/Docs-GitHub%20Pages-blue?logo=github)](https://binder-organize.github.io/Baihua-Server-Docs)

[English](README.md) | [简体中文](./docs/zh-CN/README_zh-CN.md)

Baihua is a team communication tool customized for developers, built with **Rust**, designed to help developers complete various tasks efficiently.

---

## Table of Contents
- [Table of Contents](#table-of-contents)
- [Why We Need Baihua](#why-we-need-baihua)
- [Quick Start](#quick-start)
- [How to Participate in Baihua's Development](#how-to-participate-in-baihuas-development)
- [Special Thanks](#special-thanks)
- [Contributors](#contributors)
- [FAQ](#faq)
- [License](#license)

---

## Why We Need Baihua
In today's development work, we often need to switch frequently between multiple tools and platforms: for example, operating Git in the terminal, checking project Issues in the browser, managing CI/CD in another panel, and tracking progress using spreadsheets or separate tools... This fragmented workflow not only reduces efficiency but also interrupts the developer's most precious state of 'flow'.

Baihua is born to end this fragmented experience. It is a team communication tool customized for developers, deeply integrating the tools you use daily—such as Git, GitHub/GitLab, dependency management, and build systems—unifying them into a coherent workflow.

---

## Quick Start

### Prerequisites

- **Docker** (for PostgreSQL)
- **Rust toolchain** (stable, with rustfmt + clippy)
- **Python 3.10+** and `pip install -r tests/requirements.txt` (for tests)

### Development

```bash
# 1. Set up environment variables
cp .env.example .env

# 2. Start PostgreSQL via Docker
docker compose up -d database

# 3. Start the server (auto-runs migrations on first start)
cargo run
```

The server starts on `http://localhost:2424`. An interactive console is available in the terminal — type `help` for commands.

To run the full test suite (build + DB + server + pytest):

```bash
python3 tests/run_tests.py
```

### Production

Deploy the entire stack with Docker Compose:

```bash
# 1. Prepare production environment variables
cp .env.production.example .env.production
# Then edit .env.production with your production values.

# 2. Build and start everything (first build may take 10-15 min)
docker compose --env-file .env.production --profile production up -d --build
```

> The first build downloads and compiles all Rust dependencies from scratch inside Docker.
> Later build times depend on which build layers remain cached.
> To watch build progress, use `docker compose --env-file .env.production --profile production up --build` (without `-d`).

Before starting a public deployment, check these items:

- Install Docker and Docker Compose on the deployment host.
- Point the `BAIHUA_DOMAIN` address to the deployment host before starting the public profile.
- Allow incoming traffic on ports 80 and 443 when the public profile is used.
- Replace `POSTGRES_PASSWORD` and `JWT_SECRET` with strong random values.
- Keep `.env.production` outside version control.
- Back up the database, avatar, and file volumes before upgrades.

This starts two services:

| Service | Container | Port |
|---------|-----------|------|
| **database** | `baihua-database` | 2423 (localhost only) |
| **server** | `baihua-server` | 2424 (localhost only) |

Both ports are limited to the deployment host. The database is reachable from the server over the Compose network, so it does not need a public port. Uploaded avatars are stored in the `baihua-avatar-data` volume; file message bytes are stored in `baihua-file-data`; database records are stored in `baihua-postgres-data`.

To accept public traffic, set `BAIHUA_DOMAIN` in `.env.production` to a domain whose address points to this host, allow incoming connections on ports 80 and 443, and start the optional reverse proxy:

```bash
docker compose --env-file .env.production --profile production --profile public up -d --build
```

The reverse proxy manages encrypted certificates and forwards WebSocket connections. Its certificate state is stored in `baihua-caddy-data`; preserve this volume during updates. Without the public profile, the service remains available only on the deployment host.

If you already use another reverse proxy, load balancer, or ingress controller, keep the public profile disabled. Start only the production profile and forward traffic from your existing proxy to `127.0.0.1:2424` on the deployment host. Make sure it forwards WebSocket upgrade requests to the same address.

The server is gated by the database health check and includes a Docker HEALTHCHECK (`GET /health`). Logs:

```bash
docker compose --env-file .env.production --profile production --profile public logs -f
```

To stop the stack while keeping database records, uploaded avatars, file message bytes, and certificates:

```bash
docker compose --env-file .env.production --profile production --profile public down
```

### Upgrade an existing deployment

Back up production data first, then update the repository and rebuild the containers:

```bash
git pull
docker compose --env-file .env.production --profile production --profile public up -d --build
```

The server runs database migrations during startup. After the upgrade, check service health and inspect recent logs:

```bash
curl -fsS http://127.0.0.1:2424/health
docker compose --env-file .env.production --profile production --profile public logs --tail=100
```

If you do not use the public profile, omit `--profile public` from the upgrade and log commands.

### Back up and restore production data

Back up the database, uploaded avatars, and file message bytes during a maintenance window. Stop the server first so local files and database records stay in sync, then restart it after the backup commands, including when a backup command fails. The following commands create a separate, private directory for each backup; copy it to protected storage on another host and periodically test a full restore. Keep `.env.production` and any custom server configuration in protected storage as well.

```bash
umask 077
backup_directory="backups/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$backup_directory"
docker compose --env-file .env.production --profile production stop server
docker compose --env-file .env.production exec -T database sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc' > "$backup_directory/database.dump"
docker run --rm --mount type=volume,src=baihua-avatar-data,dst=/avatars,readonly --mount "type=bind,src=$PWD/$backup_directory,dst=/backups" alpine tar -C /avatars -cf /backups/avatars.tar .
docker run --rm --mount type=volume,src=baihua-file-data,dst=/files,readonly --mount "type=bind,src=$PWD/$backup_directory,dst=/backups" alpine tar -C /files -cf /backups/files.tar .
docker compose --env-file .env.production --profile production start server
```

If the public reverse proxy is enabled, also back up its certificate state into the same directory:

```bash
docker run --rm --mount type=volume,src=baihua-caddy-data,dst=/caddy-data,readonly --mount "type=bind,src=$PWD/$backup_directory,dst=/backups" alpine tar -C /caddy-data -cf /backups/caddy-data.tar .
```

Restore into a freshly provisioned stack with an empty database and empty data volumes. Select an existing backup directory, start only the database, then restore the database, avatars, and file message bytes before starting the server:

```bash
backup_directory=backups/SELECTED_BACKUP_DIRECTORY
docker compose --env-file .env.production up -d --wait database
docker compose --env-file .env.production exec -T database sh -c 'pg_restore --exit-on-error --no-owner -U "$POSTGRES_USER" -d "$POSTGRES_DB"' < "$backup_directory/database.dump"
docker run --rm --mount type=volume,src=baihua-avatar-data,dst=/avatars --mount "type=bind,src=$PWD/$backup_directory,dst=/backups,readonly" alpine tar -C /avatars -xf /backups/avatars.tar
docker run --rm --mount type=volume,src=baihua-file-data,dst=/files --mount "type=bind,src=$PWD/$backup_directory,dst=/backups,readonly" alpine tar -C /files -xf /backups/files.tar
docker compose --env-file .env.production --profile production up -d server
```

If the backup includes `caddy-data.tar`, restore it to the empty certificate volume and then start the public profile:

```bash
docker run --rm --mount type=volume,src=baihua-caddy-data,dst=/caddy-data --mount "type=bind,src=$PWD/$backup_directory,dst=/backups,readonly" alpine tar -C /caddy-data -xf /backups/caddy-data.tar
docker compose --env-file .env.production --profile production --profile public up -d reverse-proxy
```

Check the server health, log in with an existing account, retrieve an uploaded avatar, and download a file message after each restore rehearsal. Stop the server before restoring data into an existing deployment; do not overwrite a live database.

---

## How to Participate in Baihua's Development
Baihua is an open-source project. We warmly welcome and highly anticipate developers from all over the world to join and participate in Baihua's development process.
You can contribute to Baihua's development in the following ways:
1.  Submit bug reports and feature suggestions: If you discover bugs or have any feature suggestions while using Baihua, please refer to the [Baihua Security Policy](SECURITY.md).
2.  Contribute code: If you have the ability and willingness to contribute code to Baihua, please refer to the [Baihua Contributor Guide](CONTRIBUTING.md).

---

## Special Thanks
We sincerely thank the following individuals for their outstanding contributions to Baihua (listed in alphabetical order by first name):
-   [Bob](https://github.com/ChepleBob30): Made many non-code contributions to Baihua's development and is Baihua's first user.

---

## Contributors
<a href="https://github.com/Binder-organize/Baihua-Server/contributors">
  <img src="https://contrib.rocks/image?repo=Binder-organize/Baihua-Server" alt="Contributors"/>
</a>

---

## FAQ
**Q1:** What development equipment does Gavin use?  
**A1:** MacBook Air M1.

**Q2:** More information about Gavin?  
**A2:** You can visit [Gavin's GitHub Profile](https://github.com/GavZheng).

**Q3:** Why choose Rust?  
**A3:** After a comprehensive evaluation of candidate languages such as Python/C++/C, Rust was confirmed as the optimal technical choice to meet the project requirements, thanks to its excellent cross-platform capabilities, memory safety features, and high execution efficiency.

---

## License
[Apache v2](LICENSE), Copyright 2026 Gavin Zheng.
