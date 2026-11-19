<p align="center">
  <img src="assets/brand/icon.svg" alt="" width="112" height="112">
</p>

# Cypher

**English** · [Русский](docs/README.ru.md)

[![Release](https://img.shields.io/github/v/release/zamik36/Cypher)](https://github.com/zamik36/Cypher/releases)
[![CI · dev](https://github.com/zamik36/Cypher/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/zamik36/Cypher/actions/workflows/ci.yml?query=branch%3Adev)
[![License](https://img.shields.io/github/license/zamik36/Cypher)](LICENSE)

Cypher is an end-to-end encrypted messenger for Windows, Android, and the browser. Send messages, files, voice notes, and short video messages. Create a profile on your device without a phone number or email address, then invite someone with a link or QR code.

[Download](https://github.com/zamik36/Cypher/releases) · [Self-host](docs/deploy.md) · [Contribute](.github/CONTRIBUTING.md) · [Report a vulnerability](.github/SECURITY.md)

## See Cypher in action

https://github.com/user-attachments/assets/cbfec943-a7a2-441b-9fc7-ece7ff782620

Anna and Mark start a chat with a one-time invite, share an image, and compare safety numbers. [Download the MP4 · 36 seconds](.github/media/cypher-demo.mp4).

## Features

- Direct messages with delivery and read receipts, quoted replies, and local deletion.
- Chunk-by-chunk file encryption and resumable transfers.
- Voice notes and short video messages.
- Profile recovery with a 24-word phrase, device linking, and history sync.
- Manual and automatic app lock, plus safety numbers to verify contacts.
- Push notifications and an anonymous mode that uses Tor in the native client.

**Beta.** Cypher supports Windows (Tauri 2), Android (Tauri mobile), and browsers (PWA with WebAssembly). The build matrix also includes Linux and macOS for testing. The roadmap includes native iOS support.

This README describes `dev` (0.3.0). See [Releases](https://github.com/zamik36/Cypher/releases) for available installers and their changes.

To run your own server, follow the [deployment guide](docs/deploy.md) for secrets, monitoring, and upgrades. The [roadmap](docs/roadmap.md) tracks what's left for release. The [changelog](docs/CHANGELOG.md) lists changes by version.

## How Cypher protects messages

Clients establish sessions with X3DH and encrypt messages with Double Ratchet. Servers route encrypted content and hold it temporarily in the recipient's inbox for offline delivery.

For messages to known contacts, the client can send through a relay using sealed sender when it has an access token and the route is available. First messages, file transfers, device sync, and fallback delivery use an authenticated gateway session. On that route, the gateway sees the sender and recipient.

The gateway sees the connected client's IP address. Anonymous mode routes native client connections through Tor. The PWA connects without Tor. A single relay hop leaves the IP address visible to an operator who controls both the relay and signaling. The [threat model](docs/threat-model.md) covers fallback delivery and other limits.

## Run locally

### Server with Docker

Install Docker with Compose and Bash. On Windows, use Git Bash.

```bash
cp .env.example .env
# Set your own passwords for Redis and all three NATS users in .env.
docker compose --project-directory . -f deploy/compose/docker-compose.yml up -d --build --wait
docker compose --project-directory . -f deploy/compose/docker-compose.yml ps
docker compose --project-directory . -f deploy/compose/docker-compose.yml exec gateway gateway health
docker compose --project-directory . -f deploy/compose/docker-compose.yml exec signaling signaling health
docker compose --project-directory . -f deploy/compose/docker-compose.yml exec relay relay health
```

Each `health` command checks `/ready` inside the container. The base Compose configuration keeps metrics ports internal. Set up Prometheus and Grafana with the [monitoring instructions](docs/deploy.md#мониторинг) and keep metrics off the public internet. Follow the [deployment guide](docs/deploy.md) to configure HTTPS and the server address for clients.

### Client development

Install the pinned [Rust toolchain](rust-toolchain.toml), Node.js 22, Docker, [just](https://github.com/casey/just), and Bash. For the PWA, add the `wasm32-unknown-unknown` target and install the `wasm-bindgen` CLI version listed in Cargo.lock. For Windows and Android builds, install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

Create `.env` and set the passwords as above. Install dependencies and start the infrastructure.

```bash
just deps
just infra
```

Keep the services running in a separate terminal.

```bash
just services
```

Start your client in another terminal.

```bash
just pwa-dev       # http://localhost:5174, builds the WASM bundle
# or
just desktop-dev   # uses the Tauri CLI from the project's npm dependencies
# or
just android-dev   # requires the Android SDK/NDK and a device or emulator
```

For Windows-specific Bash and build directory settings, see [Contributing](.github/CONTRIBUTING.md#windows). Find all recipes in the [Justfile](Justfile) and examples in the [command reference](docs/commands.md).

## Tests and checks

[CI](.github/workflows/ci.yml) runs Rust lints, Miri, fuzzing, dependency and coverage checks, plus TypeScript, ESLint, Vitest, and Playwright for the frontend. End-to-end tests use Redis, NATS, the native client, and a browser.

The CI load job checks latency, errors, and resource usage with 5,000 TLS connections across two gateways and 300 WebSocket pairs (600 clients). See the [benchmark methods and dated results](docs/performance.md).

## Repository layout

| Directory                | Contents                                         |
| ------------------------ | ------------------------------------------------ |
| [`apps/`](apps/)         | Windows, Android, and PWA clients                |
| [`crates/`](crates/)     | Rust core, cryptography, protocol, and transport |
| [`packages/`](packages/) | Shared SolidJS interface                         |
| [`services/`](services/) | Gateway, signaling, and relay                    |
| [`deploy/`](deploy/)     | Server, monitoring, and backup configuration     |
| [`docs/`](docs/)         | Architecture, protocol, deployment, and plans    |
| [`tests/`](tests/)       | End-to-end and load tests                        |
| [`.github/`](.github/)   | CI, contributor guides, and issue/PR templates   |

The [brand assets guide](assets/brand/GENERATING.md) explains how to generate the app icons.

## Documentation and contributing

The technical guides below are in Russian.

| Guide                                       | Covers                                |
| ------------------------------------------- | ------------------------------------- |
| [Architecture](docs/architecture.md)        | Services, client core, and storage    |
| [Protocol v4](docs/protocol-v4.md)          | Formats and message exchange          |
| [Threat model](docs/threat-model.md)        | Security guarantees and limits        |
| [Deployment](docs/deploy.md)                | Server, secrets, monitoring, releases |
| [Roadmap](docs/roadmap.md)                  | Completed work and remaining tasks    |
| [Historical assessment](docs/assessment.md) | Project snapshot from October 4, 2026 |

Report bugs and suggest features in [Issues](https://github.com/zamik36/Cypher/issues). Read the [contributing guide](.github/CONTRIBUTING.md) and [code of conduct](.github/CODE_OF_CONDUCT.md) before opening a pull request. Report vulnerabilities using the [security policy](.github/SECURITY.md).

## License

[MIT](LICENSE).
