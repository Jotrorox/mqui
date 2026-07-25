# Contributing to MQUI

Thank you for your interest in contributing to MQUI! 
I appreciate your will in helping out me and the community, 
but please still read through this small document to ensure a clean process in
working with your contribution.

## Getting Started

### Prerequisites

- The Rust toolchain >= v1.88.0
- The EGUI build dependencies ([GitHub](https://github.com/emilk/egui))
- Git for version control
- Docker with the Compose plugin, for the real-broker integration tests

On Debian/Ubuntu, the native packages used by CI are:

```bash
sudo apt-get install libasound2-dev libgtk-3-dev libudev-dev libxkbcommon-dev
```

Windows builds use the standard Rust MSVC toolchain and the Visual Studio C++
build tools. OpenSSL is only used inside the Mosquitto test container.

### Setting Up Your Development Environment

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/mqui.git
   cd mqui
   ```
3. Build and run the project:
   ```bash
   cargo run
   ```

## How to Contribute

### Reporting Bugs

If you find a bug, please open an issue on GitHub with:

- A clear, descriptive title
- Steps to reproduce the issue
- Expected vs. actual behavior
- Server version and plugin version
- Any relevant error messages or logs

### Suggesting Features

Feature suggestions are welcome! Please open an issue with:

- A clear description of the feature
- Why this feature would be useful
- Any implementation ideas you might have

### Pull Requests

1. Create a new branch for your feature or fix:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. Make your changes following our coding standards

3. Run the validation and integration-test commands documented below

4. Commit your changes with clear, descriptive commit messages:
   ```bash
   git commit -m "Add: description of your changes"
   ```

5. Push to your fork:
   ```bash
   git push origin feature/your-feature-name
   ```

6. Open a Pull Request on GitHub with:
   - A clear description of what your PR does
   - Any related issue numbers (e.g., "Fixes #123")
   - Screenshots or examples if relevant

## Code & Community Standards

### Code Style

TBD

## Testing

Fast unit tests do not need a broker:

```bash
cargo test --all-targets
```

The real-broker suite uses the pinned Mosquitto container and covers TCP,
authentication acceptance and rejection, subscribe/unsubscribe, QoS 0/1/2,
retained messages, duplicate-client disconnects, TLS with a test CA, and
WebSockets:

```bash
docker compose -f test-broker/docker-compose.yml up --detach --wait
MQUI_INTEGRATION_TESTS=1 cargo test --test mosquitto -- --nocapture
docker compose -f test-broker/docker-compose.yml down --volumes
```

The broker generates a short-lived CA and a `localhost` server certificate in
`test-broker/generated/`. The TLS test explicitly trusts that CA through
`TlsVerificationMode::CustomCa`; certificate and hostname verification remain
enabled. Set `MQUI_TEST_CA` if the generated CA is stored elsewhere.

Run the same validation commands used in CI before opening a pull request:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
MQUI_INTEGRATION_TESTS=1 cargo test --all-targets
cargo check --all-targets --all-features
```

The integration test has bounded waits, creates unique client IDs and topic
names, and is skipped unless `MQUI_INTEGRATION_TESTS` is set. CI starts and
health-checks the broker before enabling it and always tears the broker down.

## Communication

- Be respectful and constructive in all interactions
- Ask questions if something is unclear
- Be patient while waiting for reviews

## License

By contributing to MQUI, you agree that your contributions will be licensed under the MIT License.

---

Thank you for contributing to MQUI! Your help makes this project better for everyone.
