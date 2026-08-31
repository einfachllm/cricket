import requests
import json

PROXY_URL = "http://127.0.0.1:8081/v1/messages"
API_KEY = "your_anthropic_api_key_here"

headers = {
    "x-api-key": API_KEY,
    "anthropic-version": "2023-06-01",
    "Content-Type": "application/json",
    "X-Agent-ID": "opencode",
    "X-Session-ID": "test-session-001"
}

data = {
    "model": "claude-sonnet-4-5",
    "max_tokens": 256,
    "messages": [{"role": "user", "content": "Hello, how are you?"}]
}

try:
    response = requests.post(PROXY_URL, headers=headers, json=data)
    print(f"Status Code: {response.status_code}")
    print("Response JSON:")
    print(json.dumps(response.json(), indent=2))
except Exception as e:
    print(f"Error: {e}")
