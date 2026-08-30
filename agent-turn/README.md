# Agent-Turn Backend

A lightweight proxy server for monitoring agent telemetry (tokens, tool calls, latency) in real-time.

## Features
- **Proxy Mode**: Intercepts requests to LLM providers.
- **Telemetry**: Extracts token usage and tool call counts.
- **Persistence**: Stores all metrics in a local SQLite database.
- **Multi-Agent Support**: Uses headers to distinguish between different agents and sessions.

## Setup

1. **Install Rust**: Ensure you have `cargo` installed.
2. **Run the Backend**:
   ```bash
   cd backend
   cargo run
   ```

## Usage

To use the proxy, configure your agent to use `http://localhost:8080/v1/chat/completions` as its API endpoint.

### Required Headers
The agent must include the following headers for telemetry:
- `X-Agent-ID`: Name of the agent (e.g., `kilo`).
- `X-Session-ID`: Unique identifier for the current task/session.

## Testing

Use the provided test script (after adding your API key):
```bash
python3 test_client.py
```
