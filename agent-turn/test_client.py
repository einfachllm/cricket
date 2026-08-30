import requests
import json

PROXY_URL = "http://127.0.0.1:8080/v1/chat/completions"
API_KEY = "your_openai_api_key_here"

headers = {
    "Authorization": f"Bearer {API_KEY}",
    "Content-Type": "application/json",
    "X-Agent-ID": "kilo",
    "X-Session-ID": "test-session-001"
}

data = {
    "model": "gpt-3.5-turbo",
    "messages": [{"role": "user", "content": "Hello, how are you?"}],
    "temperature": 0.7
}

try:
    response = requests.post(PROXY_URL, headers=headers, json=data)
    print(f"Status Code: {response.status_code}")
    print("Response JSON:")
    print(json.dumps(response.json(), indent=2))
except Exception as e:
    print(f"Error: {e}")
