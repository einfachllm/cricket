import subprocess
import time
import requests
import os
import sqlite3

def run_test():
    print("Starting backend server...")
    # Use cargo run to start the backend.
    # We'll use a separate process.
    process = subprocess.Popen(
        ["cargo", "run"],
        cwd="agent-turn/backend",
        env={**os.environ, "BIND_ADDR": "127.0.0.1:8081"}
    )
    
    # Wait for server to be ready
    time.sleep(3)

    url = "http://127.0.0.1:8081/v1/chat/completions"
    headers = {
        "Authorization": "Bearer dummy_key",
        "Content-Type": "application/json",
        "X-Agent-ID": "test_agent",
        "X-Session-ID": "integration-test-session"
    }
    data = {
        "model": "gpt-3.5-turbo",
        "messages": [{"role": "user", "content": "Hello"}]
    }

    print(f"Sending request to {url}...")
    try:
        response = requests.post(url, headers=headers, json=data, timeout=10)
        print(f"Response status: {response.status_code}")
        # We expect an error because of the dummy key, but we want to see if it's a 401/403 (OpenAI error)
        # rather than a 500 (our server error).
        if response.status_code in [401, 403]:
            print("Success: Received expected authentication error from upstream.")
        elif response.status_code == 200:
            print("Success: Received 200 OK.")
        else:
            print(f"Warning: Received unexpected status code: {response.status_code}")
            print(response.text)

        # Check if database was updated
        db_path = "agent-turn/backend/agent_turn.db"
        if os.path.exists(db_path):
            conn = sqlite3.connect(db_path)
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) FROM agents WHERE name = 'test_agent'")
            count = cursor.fetchone()[0]
            print(f"Agent 'test_agent' count in DB: {count}")
            
            cursor.execute("SELECT COUNT(*) FROM tasks")
            task_count = cursor.fetchone()[0]
            print(f"Total tasks in DB: {task_count}")
            
            cursor.execute("SELECT COUNT(*) FROM metrics")
            metrics_count = cursor.fetchone()[0]
            print(f"Total metrics in DB: {metrics_count}")
            conn.close()
        else:
            print("Database file not found.")

    except Exception as e:
        print(f"An error occurred: {e}")
    finally:
        print("Stopping backend server...")
        process.terminate()
        process.wait()

if __name__ == "__main__":
    run_test()
