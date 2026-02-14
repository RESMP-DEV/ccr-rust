# Changelog

All notable changes to CCR-Rust will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.1] - 2025-02-14

### Added

- **Gemini Integration** — Direct API access to Google Gemini models for context compression
  - New `gemini` provider with OpenAI-compatible endpoint
  - `gemini-3-flash-preview` model support (1M+ token context window)
  - Documentation preset for cost-effective context compression
  - Comprehensive [Gemini Integration Guide](docs/gemini-integration.md)

- **Environment Variable Expansion** — Use `${VAR_NAME}` syntax in config files
  - Automatic `.env` file loading from working directory and `~/.claude-code-router/`
  - Secure API key management without hardcoding

### Changed

- **Removed `longContext` / `longContextThreshold`** — Vestigial feature replaced by explicit presets
  - Use the `documentation` preset for context compression instead
  - Cleaner configuration with explicit routing control

- **Updated Documentation**
  - README.md: Added Gemini to supported providers
  - configuration.md: Added environment variable section, removed longContext
  - presets.md: Added built-in presets documentation

### Cost Savings

With Gemini Flash for context compression:

| Scenario | Before | After | Savings |
|----------|--------|-------|---------|
| 200K → 20K tokens | $0.60 | $0.075 | **87.5%** |

At 1000 requests/day, this saves **$500+/day**.

## [1.1.0] - 2025-02-11

### Added

- **MiniMax M2.5 Support** — Updated to latest MiniMax models
- **Enhanced Transformer Chain** — Improved reasoning extraction for DeepSeek and MiniMax
- **Dashboard Persistence** — Optional Redis-backed metrics storage

### Changed

- **Simplified Configuration** — Removed deprecated fields
- **Improved Error Handling** — Better error messages for configuration issues
- **Documentation Updates** — Clarified preset routing and tier management

## [1.0.0] - 2025-02-01

### Added

- Initial release
- Multi-provider routing with automatic failover
- OpenAI and Anthropic wire format support
- SSE streaming support
- TUI dashboard for monitoring
- Preset-based routing
- Token drift monitoring
- Prometheus metrics
